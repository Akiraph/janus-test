//! Global async-task completion delivery.

use std::time::Duration;

use janus_infrastructure::{
    clock::now_utc_str,
    id::{AsyncTaskId, ProjectId},
};
use janus_runtime::interface::{AsyncTaskProjection, LogChannel, LogCursor};
use janus_sessions::interface::{CreateTurnInput, MessageRoute, SessionModelPreference};
use serde_json::json;
use tracing::warn;

use super::Application;

const DELIVERY_LIMIT: usize = 100;
const OUTPUT_LIMIT: usize = 32_000;

pub(crate) fn spawn(state: Application) {
    tokio::spawn(async move {
        let mut settled = state.runtime().subscribe_async_task_settled();
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                result = settled.recv() => {
                    if let Ok(task_id) = result {
                        deliver_logged(&state, task_id).await;
                    }
                }
                _ = tick.tick() => {
                    match state.runtime().undelivered_terminal_task_ids(DELIVERY_LIMIT).await {
                        Ok(task_ids) => {
                            for task_id in task_ids {
                                deliver_logged(&state, task_id).await;
                            }
                        }
                        Err(error) => warn!(%error, "async task delivery scan failed"),
                    }
                }
            }
        }
    });
}

async fn deliver_logged(state: &Application, task_id: janus_infrastructure::id::AsyncTaskId) {
    if let Err(error) = deliver(state, task_id).await {
        warn!(%error, %task_id, "async task completion delivery failed");
    }
}

async fn deliver(state: &Application, task_id: AsyncTaskId) -> anyhow::Result<()> {
    let task = state.runtime().async_task(task_id).await?;
    if !task.status.is_terminal() {
        return Ok(());
    }
    let session = state.sessions().get_session(task.session_id).await?;
    let project_id: ProjectId = session.project_id.parse()?;
    let owner_id = state.projects().owner_id(project_id).await?;
    let workspace_revision = state.current_workspace_revision(task.session_id).await?;
    let output = task_output(state, &task).await?;
    let now = now_utc_str();
    let actor = json!({"kind": "system", "source": "async_task", "task_id": task.id.to_string()});
    let content = format!(
        "Global async bash task {} finished.\nstatus: {}\ncommand: {}\n{}",
        task.id,
        task.status.as_str(),
        task.command_summary,
        output,
    );
    let metadata = json!({
        "task_id": task.id.to_string(),
        "status": task.status.as_str(),
        "command": task.command_summary,
        "output": output,
        "display_name": "Async task",
    });

    let mut work = state.unit_of_work().begin().await?;
    if !state
        .runtime()
        .claim_task_delivery_in_tx(work.connection(), task_id)
        .await?
    {
        work.rollback().await?;
        return Ok(());
    }
    let command = state
        .sessions()
        .lock_session_command_in_tx(
            work.connection(),
            task.session_id,
            &session.version,
            None,
            &now,
        )
        .await?;
    let preference = command
        .next_model_ref
        .as_deref()
        .map(serde_json::from_str::<SessionModelPreference>)
        .transpose()?;
    let model_snapshot = state
        .execution_coordinator()
        .resolve_model_snapshot_in_tx(
            work.connection(),
            project_id,
            Some(&owner_id),
            preference.as_ref(),
        )
        .await
        .map_err(|error| anyhow::anyhow!("resolve async task Turn model: {error}"))?;
    let has_queued = state
        .sessions()
        .has_queued_turn_in_tx(work.connection(), task.session_id)
        .await?;
    let route = if command.active_turn_id.is_some() || has_queued {
        MessageRoute::Queued
    } else {
        MessageRoute::Started
    };
    let checkpoint_revision =
        (route == MessageRoute::Started).then_some(workspace_revision.as_str());
    let created = state
        .sessions()
        .create_turn_input_in_tx(
            work.connection(),
            CreateTurnInput {
                session_id: task.session_id,
                content: &content,
                actor: &actor,
                message_kind: "system",
                timeline_kind: "async_task_result",
                metadata: Some(&metadata),
                goal_mode: true,
                predecessor_turn_id: None,
                attachment_ids: &[],
                model_snapshot: model_snapshot.as_ref(),
                checkpoint_revision,
                now: &now,
            },
        )
        .await?;
    if route == MessageRoute::Started {
        let activated = state
            .sessions()
            .activate_created_turn_in_tx(
                work.connection(),
                task.session_id,
                &created.turn_id,
                model_snapshot.as_ref(),
                &now,
            )
            .await?;
        if !activated {
            return Err(anyhow::anyhow!(
                "session became active while delivering task"
            ));
        }
    }
    for event in Application::message_accepted_events(
        task.session_id,
        &created,
        &command,
        route,
        "async_task_result",
        actor,
    ) {
        work.append_event(event).await?;
    }
    state
        .runtime()
        .complete_task_delivery_in_tx(work.connection(), task_id)
        .await?;
    let scheduled = (route == MessageRoute::Started)
        .then(|| created.turn_id.parse::<janus_infrastructure::id::TurnId>())
        .transpose()?;
    if let Some(turn_id) = scheduled {
        state.enqueue_turn_wake_in_tx(&mut work, turn_id).await?;
    }
    work.commit().await?;
    if let Some(turn_id) = scheduled {
        state.execution_coordinator().schedule(turn_id);
    }
    Ok(())
}

async fn task_output(state: &Application, task: &AsyncTaskProjection) -> anyhow::Result<String> {
    let range = state
        .runtime()
        .log_range(task.log_stream_id, LogCursor::ZERO, OUTPUT_LIMIT)
        .await?;
    let mut output = String::new();
    for chunk in range.chunks {
        let prefix = match chunk.channel {
            LogChannel::Stdout => "stdout",
            LogChannel::Stderr => "stderr",
            LogChannel::System => "system",
        };
        output.push_str("--- ");
        output.push_str(prefix);
        output.push_str(" ---\n");
        output.push_str(&chunk.text);
    }
    Ok(truncate_output(output))
}

fn truncate_output(mut output: String) -> String {
    if output.len() <= OUTPUT_LIMIT {
        return output;
    }
    let mut end = OUTPUT_LIMIT;
    while end > 0 && !output.is_char_boundary(end) {
        end -= 1;
    }
    output.truncate(end);
    output.push_str("\n...[truncated]");
    output
}

#[cfg(test)]
mod tests {
    use super::{OUTPUT_LIMIT, truncate_output};

    #[test]
    fn output_truncation_preserves_utf8_boundaries() {
        let mut output = "a".repeat(OUTPUT_LIMIT - 1);
        output.push('界');
        output.push_str("tail");

        let truncated = truncate_output(output);

        assert!(truncated.ends_with("\n...[truncated]"));
        assert!(truncated.is_char_boundary(truncated.len()));
    }
}
