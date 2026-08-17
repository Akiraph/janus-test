use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};

use futures_util::future::BoxFuture;
use janus_infrastructure::{
    id::{AsyncTaskId, LogStreamId, RuntimeId, TerminalId},
    secrets::random_token,
    shell::{bash_program, decode_process_output},
};
use janus_runtime::interface::{
    AsyncTaskSpec, ExecutionMode, ExecutionResult, ExecutionSpec, ExecutorProcessHandle,
    ExecutorRuntimeHandle, ExitSummary, LogChannel, LogRetention, LogStore, ProcessCompletion,
    ResourceUsage, RuntimeError, RuntimeExecutor, RuntimeSpec, TerminalSignal, TerminalSize,
    TerminalSpec,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{Child, Command},
    sync::{RwLock, mpsc, oneshot, watch},
    task::JoinHandle,
};

const OUTPUT_SUMMARY_BYTES: usize = 1024 * 1024;

#[derive(Clone)]
pub(crate) struct LocalExecutor {
    inner: Arc<LocalExecutorInner>,
}

struct LocalExecutorInner {
    logs: LogStore,
    runtimes: RwLock<HashMap<RuntimeId, LocalRuntime>>,
    async_tasks: RwLock<HashMap<AsyncTaskId, ManagedProcess>>,
    terminals: RwLock<HashMap<TerminalId, ManagedProcess>>,
}

#[derive(Clone)]
struct LocalRuntime {
    workspace_root: PathBuf,
    nonce: String,
}

#[derive(Clone)]
struct ManagedProcess {
    nonce: String,
    process_identity: String,
    control: mpsc::Sender<ProcessCommand>,
    completion: watch::Receiver<Option<ProcessCompletion>>,
}

enum ProcessCommand {
    Stdin(Vec<u8>, oneshot::Sender<Result<(), RuntimeError>>),
    Terminate(oneshot::Sender<()>),
    /// Best-effort interrupt: write a control byte (Ctrl-C) to stdin. Awaiting
    /// receipt confirms the write completed, not that the shell reacted.
    Interrupt(oneshot::Sender<Result<(), RuntimeError>>),
}

struct PreparedCommand {
    command: Command,
    secret_values: Vec<String>,
    process_group_marker: Option<PathBuf>,
}

struct ProcessMonitor {
    child: Child,
    stdin: Option<tokio::process::ChildStdin>,
    commands: mpsc::Receiver<ProcessCommand>,
    completion: watch::Sender<Option<ProcessCompletion>>,
    stdout_task: JoinHandle<Vec<u8>>,
    stderr_task: JoinHandle<Vec<u8>>,
    logs: LogStore,
    log_stream_id: LogStreamId,
    pid: u32,
    process_group_id: Option<u32>,
    started: Instant,
}

struct TerminalMonitor {
    child: Child,
    stdin: Option<tokio::process::ChildStdin>,
    commands: mpsc::Receiver<ProcessCommand>,
    completion: watch::Sender<Option<ProcessCompletion>>,
    stdout_task: JoinHandle<()>,
    stderr_task: JoinHandle<()>,
    logs: LogStore,
    scrollback_stream_id: LogStreamId,
    pid: u32,
    process_group_id: Option<u32>,
    started: Instant,
}

impl LocalExecutor {
    pub(crate) fn new(logs: LogStore) -> Self {
        Self {
            inner: Arc::new(LocalExecutorInner {
                logs,
                runtimes: RwLock::new(HashMap::new()),
                async_tasks: RwLock::new(HashMap::new()),
                terminals: RwLock::new(HashMap::new()),
            }),
        }
    }

    async fn runtime(&self, id: RuntimeId) -> Result<LocalRuntime, RuntimeError> {
        self.inner
            .runtimes
            .read()
            .await
            .get(&id)
            .cloned()
            .ok_or_else(|| RuntimeError::unavailable(format!("runtime handle {id} was not found")))
    }

    async fn prepare(
        &self,
        spec: &ExecutionSpec,
        _mode: ExecutionMode,
    ) -> Result<PreparedCommand, RuntimeError> {
        let runtime = self.runtime(spec.runtime_id()).await?;
        let working_directory = runtime
            .workspace_root
            .join(spec.working_directory().as_str());
        let canonical_working = tokio::fs::canonicalize(&working_directory)
            .await
            .map_err(|_| RuntimeError::InvalidSpec("working directory does not exist".into()))?;

        let process_group_marker_name = if cfg!(windows) {
            Some(format!(".janus-runtime-{}.pid", random_token(12)))
        } else {
            None
        };
        let command_input = if process_group_marker_name.is_some() {
            format!(
                "printf '%s\\n' \"$$\" > \"$JANUS_RUNTIME_PID_FILE\"; {}",
                spec.command().input()
            )
        } else {
            spec.command().input().to_owned()
        };
        let mut command = shell_command(&command_input);
        command
            .current_dir(&canonical_working)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (name, value) in spec.environment().ordinary() {
            command.env(name, value);
        }
        let secret_values = spec
            .environment()
            .secrets()
            .iter()
            .map(|value| value.value().expose().to_owned())
            .collect::<Vec<_>>();
        for value in spec.environment().secrets() {
            command.env(value.name(), value.value().expose());
        }
        if let Some(marker) = process_group_marker_name.as_deref() {
            command.env("JANUS_RUNTIME_PID_FILE", marker);
        }
        Ok(PreparedCommand {
            command,
            secret_values,
            process_group_marker: process_group_marker_name
                .map(|marker| canonical_working.join(marker)),
        })
    }

    async fn start_managed(
        &self,
        spec: &ExecutionSpec,
        mode: ExecutionMode,
        log_stream_id: LogStreamId,
    ) -> Result<ManagedProcess, RuntimeError> {
        let runtime = self.runtime(spec.runtime_id()).await?;
        let mut prepared = self.prepare(spec, mode).await?;
        let process_group_marker = prepared.process_group_marker.clone();
        let started = Instant::now();
        let mut child = prepared.command.spawn().map_err(|error| {
            RuntimeError::unavailable(format!("failed to spawn shell: {error}"))
        })?;
        let process_group_id = read_process_group_id(process_group_marker.as_deref()).await;
        remove_process_group_marker(process_group_marker.as_deref()).await;
        let pid = child
            .id()
            .ok_or_else(|| RuntimeError::unavailable("spawned shell has no process id"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| RuntimeError::unavailable("shell stdout pipe was not available"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| RuntimeError::unavailable("shell stderr pipe was not available"))?;
        let stdin = child.stdin.take();
        let stdout_task = spawn_output_reader(
            stdout,
            self.inner.logs.clone(),
            log_stream_id,
            LogChannel::Stdout,
            prepared.secret_values.clone(),
            LogRetention::ASYNC_TASK,
        );
        let stderr_task = spawn_output_reader(
            stderr,
            self.inner.logs.clone(),
            log_stream_id,
            LogChannel::Stderr,
            prepared.secret_values,
            LogRetention::ASYNC_TASK,
        );
        let (control, commands) = mpsc::channel(8);
        let (completion_tx, completion) = watch::channel(None);
        let logs = self.inner.logs.clone();
        tokio::spawn(manage_process(ProcessMonitor {
            child,
            stdin,
            commands,
            completion: completion_tx,
            stdout_task,
            stderr_task,
            logs,
            log_stream_id,
            pid,
            process_group_id,
            started,
        }));
        Ok(ManagedProcess {
            nonce: runtime.nonce,
            process_identity: pid.to_string(),
            control,
            completion,
        })
    }

    async fn wait_managed(
        process: ManagedProcess,
        expected_nonce: &str,
    ) -> Result<ProcessCompletion, RuntimeError> {
        ensure_nonce(&process, expected_nonce)?;
        let mut completion = process.completion;
        loop {
            if let Some(value) = completion.borrow().clone() {
                return Ok(value);
            }
            completion.changed().await.map_err(|error| {
                RuntimeError::unavailable(format!("runtime completion channel failed: {error}"))
            })?;
        }
    }

    async fn terminate_managed(
        process: ManagedProcess,
        expected_nonce: &str,
    ) -> Result<ProcessCompletion, RuntimeError> {
        ensure_nonce(&process, expected_nonce)?;
        if process.completion.borrow().is_none() {
            let (sent, received) = oneshot::channel();
            process
                .control
                .send(ProcessCommand::Terminate(sent))
                .await
                .map_err(|error| {
                    RuntimeError::unavailable(format!("runtime control channel failed: {error}"))
                })?;
            let _ = received.await;
        }
        Self::wait_managed(process, expected_nonce).await
    }

    async fn start_terminal_internal(
        &self,
        spec: &TerminalSpec,
        scrollback_stream_id: LogStreamId,
    ) -> Result<ManagedProcess, RuntimeError> {
        let runtime = self.runtime(spec.runtime_id).await?;
        let working_directory = runtime.workspace_root.join(spec.working_directory.as_str());
        let canonical_working = tokio::fs::canonicalize(&working_directory)
            .await
            .map_err(|_| RuntimeError::InvalidSpec("working directory does not exist".into()))?;

        // Terminal backends intentionally bypass sync-background-command policy:
        // a Terminal is a long-lived interactive shell where backgrounded async_tasks
        // and `&` are expected user input, not Execution Bash.
        let mut command = terminal_command(spec.size);
        command
            .current_dir(canonical_working)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (name, value) in spec.environment.ordinary() {
            command.env(name, value);
        }
        let secret_values = spec
            .environment
            .secrets()
            .iter()
            .map(|value| value.value().expose().to_owned())
            .collect::<Vec<_>>();
        for value in spec.environment.secrets() {
            command.env(value.name(), value.value().expose());
        }

        let mut child = command.spawn().map_err(|error| {
            tracing::warn!(%error, "terminal shell spawn failed");
            RuntimeError::unavailable(format!("failed to spawn terminal shell: {error}"))
        })?;
        let pid = child
            .id()
            .ok_or_else(|| RuntimeError::unavailable("terminal shell has no process id"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| RuntimeError::unavailable("terminal stdout pipe was not available"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| RuntimeError::unavailable("terminal stderr pipe was not available"))?;
        let stdin = child.stdin.take();
        let stdout_task = spawn_terminal_reader(
            stdout,
            self.inner.logs.clone(),
            scrollback_stream_id,
            LogChannel::Stdout,
            secret_values.clone(),
        );
        let stderr_task = spawn_terminal_reader(
            stderr,
            self.inner.logs.clone(),
            scrollback_stream_id,
            LogChannel::Stderr,
            secret_values,
        );
        let (control, commands) = mpsc::channel(16);
        let (completion_tx, completion) = watch::channel(None);
        let logs = self.inner.logs.clone();
        let started = Instant::now();
        tokio::spawn(manage_terminal(TerminalMonitor {
            child,
            stdin,
            commands,
            completion: completion_tx,
            stdout_task,
            stderr_task,
            logs,
            scrollback_stream_id,
            pid,
            process_group_id: None,
            started,
        }));
        Ok(ManagedProcess {
            nonce: runtime.nonce,
            process_identity: pid.to_string(),
            control,
            completion,
        })
    }
}

impl RuntimeExecutor for LocalExecutor {
    fn ensure_runtime<'a>(
        &'a self,
        spec: &'a RuntimeSpec,
    ) -> BoxFuture<'a, Result<ExecutorRuntimeHandle, RuntimeError>> {
        Box::pin(async move {
            if let Some(existing) = self.inner.runtimes.read().await.get(&spec.id()).cloned() {
                return Ok(ExecutorRuntimeHandle {
                    executor_identity: format!("local:{}", spec.id()),
                    executor_nonce: existing.nonce,
                });
            }
            let workspace_root = tokio::fs::canonicalize(spec.workspace_root())
                .await
                .map_err(|_| RuntimeError::InvalidSpec("workspace root does not exist".into()))?;
            let nonce = random_token(24);
            let handle = LocalRuntime {
                workspace_root,
                nonce: nonce.clone(),
            };
            self.inner.runtimes.write().await.insert(spec.id(), handle);
            Ok(ExecutorRuntimeHandle {
                executor_identity: format!("local:{}", spec.id()),
                executor_nonce: nonce,
            })
        })
    }

    fn stop_runtime<'a>(
        &'a self,
        runtime_id: RuntimeId,
        executor_nonce: &'a str,
    ) -> BoxFuture<'a, Result<(), RuntimeError>> {
        Box::pin(async move {
            let current = self.runtime(runtime_id).await?;
            if current.nonce != executor_nonce {
                return Err(RuntimeError::RuntimeUnavailable);
            }
            self.inner.runtimes.write().await.remove(&runtime_id);
            Ok(())
        })
    }

    fn execute_sync<'a>(
        &'a self,
        spec: ExecutionSpec,
        log_stream_id: LogStreamId,
    ) -> BoxFuture<'a, Result<ExecutionResult, RuntimeError>> {
        Box::pin(async move {
            let mut prepared = self.prepare(&spec, ExecutionMode::Sync).await?;
            let process_group_marker = prepared.process_group_marker.clone();
            let started = Instant::now();
            let mut child = prepared.command.spawn().map_err(|error| {
                RuntimeError::unavailable(format!("failed to spawn sync shell: {error}"))
            })?;
            let process_group_id = read_process_group_id(process_group_marker.as_deref()).await;
            remove_process_group_marker(process_group_marker.as_deref()).await;
            let pid = child
                .id()
                .ok_or_else(|| RuntimeError::unavailable("sync shell has no process id"))?;
            let stdout = child
                .stdout
                .take()
                .ok_or_else(|| RuntimeError::unavailable("sync stdout pipe was not available"))?;
            let stderr = child
                .stderr
                .take()
                .ok_or_else(|| RuntimeError::unavailable("sync stderr pipe was not available"))?;
            drop(child.stdin.take());
            let stdout_task = spawn_output_reader(
                stdout,
                self.inner.logs.clone(),
                log_stream_id,
                LogChannel::Stdout,
                prepared.secret_values.clone(),
                LogRetention::ASYNC_TASK,
            );
            let stderr_task = spawn_output_reader(
                stderr,
                self.inner.logs.clone(),
                log_stream_id,
                LogChannel::Stderr,
                prepared.secret_values,
                LogRetention::ASYNC_TASK,
            );
            let timeout = std::time::Duration::from_millis(spec.limits().timeout_ms);
            let (status, timed_out) = match tokio::time::timeout(timeout, child.wait()).await {
                Ok(Ok(status)) => (Some(status), false),
                Ok(Err(_)) => (None, false),
                Err(_) => {
                    terminate_process_tree(&mut child, pid, process_group_id).await;
                    (child.wait().await.ok(), true)
                }
            };
            if !timed_out {
                cleanup_descendants(pid, process_group_id).await;
            }
            let stdout = join_output(stdout_task).await;
            let stderr = join_output(stderr_task).await;
            let stream = self.inner.logs.close(log_stream_id).await?;
            let stdout = decode_process_output(&stdout, OUTPUT_SUMMARY_BYTES);
            let stderr = decode_process_output(&stderr, OUTPUT_SUMMARY_BYTES);
            Ok(ExecutionResult {
                mode: ExecutionMode::Sync,
                exit: exit_summary(status.as_ref()),
                stdout: stdout.text,
                stderr: stderr.text,
                log_stream_id,
                output_cursor: stream.next_cursor,
                duration_ms: elapsed_millis(started),
                usage: ResourceUsage::default(),
                timed_out,
                truncated: stream.truncated || stdout.truncated || stderr.truncated,
            })
        })
    }

    fn start_async_task<'a>(
        &'a self,
        spec: AsyncTaskSpec,
        log_stream_id: LogStreamId,
    ) -> BoxFuture<'a, Result<ExecutorProcessHandle, RuntimeError>> {
        Box::pin(async move {
            let id = spec.id;
            let process = self
                .start_managed(&spec.execution, ExecutionMode::AsyncTask, log_stream_id)
                .await?;
            let result = ExecutorProcessHandle {
                process_identity: process.process_identity.clone(),
                executor_nonce: process.nonce.clone(),
            };
            self.inner.async_tasks.write().await.insert(id, process);
            Ok(result)
        })
    }

    fn wait_async_task<'a>(
        &'a self,
        id: AsyncTaskId,
        executor_nonce: &'a str,
    ) -> BoxFuture<'a, Result<ProcessCompletion, RuntimeError>> {
        Box::pin(async move {
            let process = self
                .inner
                .async_tasks
                .read()
                .await
                .get(&id)
                .cloned()
                .ok_or(RuntimeError::AsyncTaskLost(id))?;
            Self::wait_managed(process, executor_nonce).await
        })
    }

    fn write_async_task_stdin<'a>(
        &'a self,
        id: AsyncTaskId,
        executor_nonce: &'a str,
        input: Vec<u8>,
    ) -> BoxFuture<'a, Result<(), RuntimeError>> {
        Box::pin(async move {
            let process = self
                .inner
                .async_tasks
                .read()
                .await
                .get(&id)
                .cloned()
                .ok_or(RuntimeError::AsyncTaskLost(id))?;
            ensure_nonce(&process, executor_nonce)?;
            let (sent, received) = oneshot::channel();
            process
                .control
                .send(ProcessCommand::Stdin(input, sent))
                .await
                .map_err(|_| RuntimeError::AsyncTaskLost(id))?;
            received
                .await
                .map_err(|_| RuntimeError::AsyncTaskLost(id))?
        })
    }

    fn cancel_async_task<'a>(
        &'a self,
        id: AsyncTaskId,
        executor_nonce: &'a str,
    ) -> BoxFuture<'a, Result<ProcessCompletion, RuntimeError>> {
        Box::pin(async move {
            let process = self
                .inner
                .async_tasks
                .read()
                .await
                .get(&id)
                .cloned()
                .ok_or(RuntimeError::AsyncTaskLost(id))?;
            Self::terminate_managed(process, executor_nonce).await
        })
    }

    fn start_terminal<'a>(
        &'a self,
        spec: TerminalSpec,
        scrollback_stream_id: LogStreamId,
    ) -> BoxFuture<'a, Result<ExecutorProcessHandle, RuntimeError>> {
        Box::pin(async move {
            let id = spec.id;
            let process = self
                .start_terminal_internal(&spec, scrollback_stream_id)
                .await?;
            let result = ExecutorProcessHandle {
                process_identity: process.process_identity.clone(),
                executor_nonce: process.nonce.clone(),
            };
            self.inner.terminals.write().await.insert(id, process);
            Ok(result)
        })
    }

    fn write_terminal_input<'a>(
        &'a self,
        id: TerminalId,
        executor_nonce: &'a str,
        input: Vec<u8>,
    ) -> BoxFuture<'a, Result<(), RuntimeError>> {
        Box::pin(async move {
            let process = self
                .inner
                .terminals
                .read()
                .await
                .get(&id)
                .cloned()
                .ok_or(RuntimeError::TerminalNotWritable(id))?;
            ensure_nonce(&process, executor_nonce)?;
            if process.completion.borrow().is_some() {
                return Err(RuntimeError::TerminalNotWritable(id));
            }
            if input.is_empty() {
                return Ok(());
            }
            let (sent, received) = oneshot::channel();
            process
                .control
                .send(ProcessCommand::Stdin(input, sent))
                .await
                .map_err(|_| RuntimeError::TerminalNotWritable(id))?;
            received
                .await
                .map_err(|_| RuntimeError::TerminalNotWritable(id))?
        })
    }

    fn resize_terminal<'a>(
        &'a self,
        id: TerminalId,
        executor_nonce: &'a str,
        size: TerminalSize,
    ) -> BoxFuture<'a, Result<(), RuntimeError>> {
        Box::pin(async move {
            let process = self
                .inner
                .terminals
                .read()
                .await
                .get(&id)
                .cloned()
                .ok_or(RuntimeError::TerminalNotWritable(id))?;
            ensure_nonce(&process, executor_nonce)?;
            // The pipe backend cannot propagate a resize to a non-tty shell; the
            // durable Terminal projection records the new size either way.
            let _ = size;
            Ok(())
        })
    }

    fn signal_terminal<'a>(
        &'a self,
        id: TerminalId,
        executor_nonce: &'a str,
        signal: TerminalSignal,
    ) -> BoxFuture<'a, Result<(), RuntimeError>> {
        Box::pin(async move {
            let process = self
                .inner
                .terminals
                .read()
                .await
                .get(&id)
                .cloned()
                .ok_or(RuntimeError::TerminalNotWritable(id))?;
            ensure_nonce(&process, executor_nonce)?;
            if process.completion.borrow().is_some() {
                return Ok(());
            }
            match signal {
                TerminalSignal::Terminate => {
                    let (sent, received) = oneshot::channel();
                    process
                        .control
                        .send(ProcessCommand::Terminate(sent))
                        .await
                        .map_err(|_| RuntimeError::TerminalNotWritable(id))?;
                    let _ = received.await;
                }
                TerminalSignal::CtrlC => {
                    let (sent, received) = oneshot::channel();
                    process
                        .control
                        .send(ProcessCommand::Interrupt(sent))
                        .await
                        .map_err(|_| RuntimeError::TerminalNotWritable(id))?;
                    let _ = received.await;
                }
            }
            Ok(())
        })
    }

    fn close_terminal<'a>(
        &'a self,
        id: TerminalId,
        executor_nonce: &'a str,
    ) -> BoxFuture<'a, Result<ProcessCompletion, RuntimeError>> {
        Box::pin(async move {
            let process = self
                .inner
                .terminals
                .write()
                .await
                .remove(&id)
                .ok_or(RuntimeError::TerminalNotWritable(id))?;
            Self::terminate_managed(process, executor_nonce).await
        })
    }

    fn await_terminal_exit<'a>(
        &'a self,
        id: TerminalId,
        executor_nonce: &'a str,
    ) -> BoxFuture<'a, Result<ProcessCompletion, RuntimeError>> {
        Box::pin(async move {
            let process = self
                .inner
                .terminals
                .read()
                .await
                .get(&id)
                .cloned()
                .ok_or(RuntimeError::TerminalNotWritable(id))?;
            Self::wait_managed(process, executor_nonce).await
        })
    }
}

async fn manage_process(input: ProcessMonitor) {
    let ProcessMonitor {
        mut child,
        mut stdin,
        mut commands,
        completion,
        stdout_task,
        stderr_task,
        logs,
        log_stream_id,
        pid,
        process_group_id,
        started,
    } = input;
    let status = loop {
        tokio::select! {
            result = child.wait() => break result.ok(),
            command = commands.recv() => match command {
                Some(ProcessCommand::Stdin(input, response)) => {
                    let result = if let Some(writer) = stdin.as_mut() {
                        writer.write_all(&input).await.map_err(|_| RuntimeError::RuntimeUnavailable)
                    } else {
                        Err(RuntimeError::ResourceBusy)
                    };
                    let _ = response.send(result);
                }
                Some(ProcessCommand::Terminate(response)) => {
                    terminate_process_tree(&mut child, pid, process_group_id).await;
                    let _ = response.send(());
                }
                Some(ProcessCommand::Interrupt(response)) => {
                    let result = if let Some(writer) = stdin.as_mut() {
                        writer
                            .write_all(b"\x03")
                            .await
                            .map_err(|_| RuntimeError::RuntimeUnavailable)
                    } else {
                        Err(RuntimeError::ResourceBusy)
                    };
                    let _ = response.send(result);
                }
                None => {}
            }
        }
    };
    cleanup_descendants(pid, process_group_id).await;
    let _ = join_output(stdout_task).await;
    let _ = join_output(stderr_task).await;
    let _ = logs.close(log_stream_id).await;
    let _ = completion.send(Some(ProcessCompletion {
        exit: exit_summary(status.as_ref()),
        duration_ms: elapsed_millis(started),
        usage: ResourceUsage::default(),
    }));
}

/// Terminal shell lifecycle. Mirrors [`manage_process`] but the shell is
/// long-lived and exit happens via normal `/exit` input, `Ctrl-D`, or a
/// `Terminate` command. Output readers do not accumulate bytes; the scrollback
/// log stream is the durable record.
async fn manage_terminal(input: TerminalMonitor) {
    let TerminalMonitor {
        mut child,
        mut stdin,
        mut commands,
        completion,
        stdout_task,
        stderr_task,
        logs,
        scrollback_stream_id,
        pid,
        process_group_id,
        started,
    } = input;
    let status = loop {
        tokio::select! {
            result = child.wait() => break result.ok(),
            command = commands.recv() => match command {
                Some(ProcessCommand::Stdin(input, response)) => {
                    let result = if let Some(writer) = stdin.as_mut() {
                        writer.write_all(&input).await.map_err(|_| RuntimeError::RuntimeUnavailable)
                    } else {
                        Err(RuntimeError::RuntimeUnavailable)
                    };
                    let _ = response.send(result);
                }
                Some(ProcessCommand::Interrupt(response)) => {
                    let result = if let Some(writer) = stdin.as_mut() {
                        writer.write_all(b"\x03").await.map_err(|_| RuntimeError::RuntimeUnavailable)
                    } else {
                        Err(RuntimeError::RuntimeUnavailable)
                    };
                    let _ = response.send(result);
                }
                Some(ProcessCommand::Terminate(response)) => {
                    terminate_process_tree(&mut child, pid, process_group_id).await;
                    let _ = response.send(());
                }
                None => {}
            }
        }
    };
    cleanup_descendants(pid, process_group_id).await;
    let _ = stdout_task.await;
    let _ = stderr_task.await;
    let _ = logs.close(scrollback_stream_id).await;
    let _ = completion.send(Some(ProcessCompletion {
        exit: exit_summary(status.as_ref()),
        duration_ms: elapsed_millis(started),
        usage: ResourceUsage::default(),
    }));
}

fn spawn_terminal_reader<R>(
    mut reader: R,
    logs: LogStore,
    stream_id: LogStreamId,
    channel: LogChannel,
    secrets: Vec<String>,
) -> JoinHandle<()>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut buffer = vec![0_u8; 16 * 1024];
        loop {
            let read = match reader.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(read) => read,
            };
            let values = secrets.iter().map(String::as_str).collect::<Vec<_>>();
            let _ = logs
                .append(
                    stream_id,
                    channel,
                    &buffer[..read],
                    &values,
                    LogRetention::TERMINAL,
                )
                .await;
        }
    })
}

/// Build the interactive shell command for a Terminal. On Windows this prefers
/// Git for Windows' bundled `bash.exe`; there is no ConPTY fallback and no
/// PowerShell Terminal backend. On Unix this is `/bin/bash`. The shell runs
/// interactively so readline can interpret `Ctrl-C` bytes even without a tty.
fn terminal_command(size: TerminalSize) -> Command {
    #[cfg(windows)]
    {
        let program = bash_program().unwrap_or_else(|| {
            tracing::warn!("Git Bash could not be located; Terminal backend will refuse to spawn");
            PathBuf::from("bash")
        });
        let mut command = Command::new(program);
        command.args(["-i"]);
        command.env("COLUMNS", size.cols.to_string());
        command.env("LINES", size.rows.to_string());
        command.env("TERM", "dumb");
        command
    }
    #[cfg(not(windows))]
    {
        let mut command =
            Command::new(bash_program().unwrap_or_else(|| PathBuf::from("/bin/bash")));
        command.args(["-i"]);
        command.env("COLUMNS", size.cols.to_string());
        command.env("LINES", size.rows.to_string());
        command.env("TERM", "dumb");
        command
    }
}

fn spawn_output_reader<R>(
    mut reader: R,
    logs: LogStore,
    stream_id: LogStreamId,
    channel: LogChannel,
    secrets: Vec<String>,
    retention: LogRetention,
) -> JoinHandle<Vec<u8>>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut all = Vec::new();
        let mut buffer = vec![0_u8; 16 * 1024];
        loop {
            let read = match reader.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(read) => read,
            };
            let values = secrets.iter().map(String::as_str).collect::<Vec<_>>();
            let _ = logs
                .append(stream_id, channel, &buffer[..read], &values, retention)
                .await;
            if all.len() < OUTPUT_SUMMARY_BYTES.saturating_add(1) {
                let remaining = OUTPUT_SUMMARY_BYTES
                    .saturating_add(1)
                    .saturating_sub(all.len());
                all.extend_from_slice(&buffer[..read.min(remaining)]);
            }
        }
        all
    })
}

async fn join_output(mut task: JoinHandle<Vec<u8>>) -> Vec<u8> {
    match tokio::time::timeout(PROCESS_TERMINATION_TIMEOUT, &mut task).await {
        Ok(Ok(output)) => output,
        Ok(Err(_)) => Vec::new(),
        Err(_) => {
            task.abort();
            Vec::new()
        }
    }
}

fn ensure_nonce(process: &ManagedProcess, expected: &str) -> Result<(), RuntimeError> {
    if process.nonce == expected {
        Ok(())
    } else {
        Err(RuntimeError::RuntimeUnavailable)
    }
}

async fn read_process_group_id(path: Option<&Path>) -> Option<u32> {
    #[cfg(windows)]
    {
        let path = path?;
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if let Ok(value) = tokio::fs::read_to_string(path).await
                && let Ok(id) = value.trim().parse::<u32>()
            {
                return Some(id);
            }
            if Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        None
    }
}

async fn remove_process_group_marker(path: Option<&Path>) {
    #[cfg(windows)]
    if let Some(path) = path {
        let _ = tokio::fs::remove_file(path).await;
    }
    #[cfg(not(windows))]
    let _ = path;
}

fn shell_command(script: &str) -> Command {
    #[cfg(windows)]
    {
        // Execution Bash and Runtime shell commands must have the same
        // language semantics. Resolving the bundled executable also works
        // when Git for Windows is installed outside PATH.
        let program = bash_program().unwrap_or_else(|| PathBuf::from("bash"));
        let mut command = Command::new(program);
        command.args(["-c", script]);
        command
    }
    #[cfg(not(windows))]
    {
        let mut command = Command::new("/bin/sh");
        command.args(["-lc", script]);
        command
    }
}

#[cfg(windows)]
async fn terminate_process_tree(child: &mut Child, pid: u32, process_group_id: Option<u32>) {
    kill_msys_process_group(process_group_id).await;
    let _ = tokio::time::timeout(
        PROCESS_TERMINATION_TIMEOUT,
        Command::new("taskkill.exe")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status(),
    )
    .await;
    let _ = child.start_kill();
    let _ = tokio::time::timeout(PROCESS_TERMINATION_TIMEOUT, child.wait()).await;
}

#[cfg(not(windows))]
async fn terminate_process_tree(child: &mut Child, pid: u32, process_group_id: Option<u32>) {
    cleanup_descendants(pid, process_group_id).await;
    let _ = child.start_kill();
    let _ = tokio::time::timeout(PROCESS_TERMINATION_TIMEOUT, child.wait()).await;
    cleanup_descendants(pid, process_group_id).await;
}

#[cfg(windows)]
async fn cleanup_descendants(pid: u32, process_group_id: Option<u32>) {
    if process_group_id.is_some() {
        kill_msys_process_group(process_group_id).await;
        return;
    }
    let script = format!(
        "$root={pid}; $all=@(Get-CimInstance Win32_Process); $ids=@($root); \
         do {{ $next=@($all | Where-Object {{ $ids -contains $_.ParentProcessId }} | \
         Select-Object -ExpandProperty ProcessId); $new=@($next | Where-Object {{ $ids -notcontains $_ }}); \
         $ids += $new }} while ($new.Count -gt 0); \
         @($ids | Where-Object {{ $_ -ne $root }}) | Sort-Object -Descending | \
         ForEach-Object {{ Stop-Process -Id $_ -Force -ErrorAction SilentlyContinue }}"
    );
    let _ = tokio::time::timeout(
        PROCESS_TERMINATION_TIMEOUT,
        Command::new("powershell.exe")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &script,
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status(),
    )
    .await;
}

#[cfg(windows)]
async fn kill_msys_process_group(process_group_id: Option<u32>) {
    let Some(process_group_id) = process_group_id else {
        return;
    };
    let Some(program) = bash_program() else {
        return;
    };
    let command = format!("kill -KILL -- -{process_group_id}");
    let _ = tokio::time::timeout(
        Duration::from_millis(500),
        Command::new(program)
            .args(["--login", "-c", &command])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status(),
    )
    .await;
}

#[cfg(not(windows))]
async fn cleanup_descendants(pid: u32, _process_group_id: Option<u32>) {
    let _ = tokio::time::timeout(
        PROCESS_TERMINATION_TIMEOUT,
        Command::new("pkill")
            .args(["-TERM", "-P", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status(),
    )
    .await;
}

const PROCESS_TERMINATION_TIMEOUT: Duration = Duration::from_secs(2);

fn exit_summary(status: Option<&std::process::ExitStatus>) -> ExitSummary {
    ExitSummary {
        exit_code: status.and_then(std::process::ExitStatus::code),
        signal: status
            .filter(|value| value.code().is_none())
            .map(|_| "terminated".into()),
    }
}

fn elapsed_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, process::Stdio};

    use anyhow::Context;
    use janus_runtime::interface::{
        AsyncTaskSpec, ExecutionEnvironment, ExecutionSpec, LogChannel, LogCursor, LogOwnerKind,
        LogStore, RelativeWorkingDirectory, ResourceLimits, RuntimeError, RuntimeExecutor,
        RuntimeSpec, ValidatedCommand,
    };
    use tempfile::TempDir;
    use tokio::process::Command;

    use super::LocalExecutor;
    use janus_infrastructure::database::Database;
    use janus_infrastructure::id::{AsyncTaskId, ProjectId, SessionId, ToolCallId, TurnId};

    fn limits(timeout_ms: u64) -> ResourceLimits {
        ResourceLimits {
            timeout_ms,
            memory_bytes: 256 * 1024 * 1024,
            cpu_millis: 1_000,
            pids: 32,
            temporary_disk_bytes: 128 * 1024 * 1024,
            open_files: 128,
        }
    }

    #[tokio::test]
    async fn sync_success_failure_timeout_and_background() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let workspace = temp.path().join("workspace");
        tokio::fs::create_dir_all(&workspace).await?;
        let database = Database::open(&temp.path().join("data"), crate::migrator()).await?;
        let logs = LogStore::new(database.pool().clone(), &temp.path().join("data"));
        let executor = LocalExecutor::new(logs.clone());
        let runtime_id = janus_infrastructure::id::RuntimeId::new();
        let runtime = RuntimeSpec::new(
            runtime_id,
            janus_runtime::interface::RuntimeScope::project(ProjectId::new()),
            workspace,
            limits(5_000),
        )?;
        executor.ensure_runtime(&runtime).await?;

        let success_log = logs.create(LogOwnerKind::Sync, "success").await?;
        let success = executor
            .execute_sync(
                execution(runtime_id, success_script(), 5_000)?,
                success_log.id,
            )
            .await?;
        assert_eq!(success.exit.exit_code, Some(0));
        assert!(success.stdout.contains("local-ok"));
        assert!(!success.timed_out);
        let range = logs.read(success_log.id, LogCursor::new(0), 4096).await?;
        assert!(
            range
                .chunks
                .iter()
                .any(|chunk| chunk.channel == LogChannel::Stdout)
        );

        let failed_log = logs.create(LogOwnerKind::Sync, "failed").await?;
        let failed = executor
            .execute_sync(
                execution(runtime_id, failure_script(), 5_000)?,
                failed_log.id,
            )
            .await?;
        assert_ne!(failed.exit.exit_code, Some(0));

        let timeout_log = logs.create(LogOwnerKind::Sync, "timeout").await?;
        let timeout = executor
            .execute_sync(execution(runtime_id, sleep_script(), 50)?, timeout_log.id)
            .await?;
        assert!(timeout.timed_out);

        let background_log = logs.create(LogOwnerKind::Sync, "background").await?;
        let background = executor
            .execute_sync(
                execution(runtime_id, background_script(), 5_000)?,
                background_log.id,
            )
            .await?;
        assert_eq!(background.exit.exit_code, Some(0));
        assert!(!background.timed_out);
        Ok(())
    }

    #[tokio::test]
    async fn sync_shell_inherits_the_normal_process_environment() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let workspace = temp.path().join("workspace");
        tokio::fs::create_dir_all(&workspace).await?;
        let database = Database::open(&temp.path().join("data"), crate::migrator()).await?;
        let logs = LogStore::new(database.pool().clone(), &temp.path().join("data"));
        let executor = LocalExecutor::new(logs.clone());
        let runtime_id = janus_infrastructure::id::RuntimeId::new();
        let runtime = RuntimeSpec::new(
            runtime_id,
            janus_runtime::interface::RuntimeScope::project(ProjectId::new()),
            workspace,
            limits(5_000),
        )?;
        executor.ensure_runtime(&runtime).await?;

        let log = logs.create(LogOwnerKind::Sync, "environment").await?;
        let result = executor
            .execute_sync(
                execution(
                    runtime_id,
                    r#"printf 'USER=%s\nPATH=%s\n' "$USER" "$PATH""#,
                    5_000,
                )?,
                log.id,
            )
            .await?;

        assert!(result.stdout.lines().any(|line| line.starts_with("USER=")));
        assert!(result.stdout.lines().any(|line| line.starts_with("PATH=")));
        Ok(())
    }

    #[tokio::test]
    async fn async_task_rejects_stale_nonce_and_cleans_up_descendants() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let workspace = temp.path().join("workspace");
        tokio::fs::create_dir_all(&workspace).await?;
        let database = Database::open(&temp.path().join("data"), crate::migrator()).await?;
        let logs = LogStore::new(database.pool().clone(), &temp.path().join("data"));
        let executor = LocalExecutor::new(logs.clone());
        let runtime_id = janus_infrastructure::id::RuntimeId::new();
        let session_id = SessionId::new();
        let runtime = RuntimeSpec::new(
            runtime_id,
            janus_runtime::interface::RuntimeScope::project(ProjectId::new()),
            workspace,
            limits(30_000),
        )?;
        let runtime_handle = executor.ensure_runtime(&runtime).await?;
        let async_task_id = AsyncTaskId::new();
        let log = logs
            .create(LogOwnerKind::AsyncTask, &async_task_id.to_string())
            .await?;
        let process = executor
            .start_async_task(
                AsyncTaskSpec::new(
                    async_task_id,
                    session_id,
                    TurnId::new(),
                    ToolCallId::new(),
                    execution(runtime_id, descendant_script(), 30_000)?,
                )?,
                log.id,
            )
            .await?;
        assert_eq!(process.executor_nonce, runtime_handle.executor_nonce);
        assert!(matches!(
            executor
                .write_async_task_stdin(async_task_id, "stale-nonce", b"ignored".to_vec())
                .await,
            Err(RuntimeError::RuntimeUnavailable)
        ));

        let child_pid = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let range = logs.read(log.id, LogCursor::ZERO, 4096).await?;
                if let Some(pid) = range
                    .chunks
                    .iter()
                    .flat_map(|chunk| chunk.text.lines())
                    .find_map(|line| line.trim().parse::<u32>().ok())
                {
                    return Ok::<_, RuntimeError>(pid);
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .context("wait for descendant PID in the async_task log")??;
        assert!(process_exists(child_pid).await);

        executor
            .cancel_async_task(async_task_id, &runtime_handle.executor_nonce)
            .await?;
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while process_exists(child_pid).await {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        })
        .await
        .context("wait for descendant process cleanup")?;
        Ok(())
    }

    fn execution(
        runtime_id: janus_infrastructure::id::RuntimeId,
        script: &str,
        timeout_ms: u64,
    ) -> Result<ExecutionSpec, janus_runtime::interface::RuntimeError> {
        ExecutionSpec::new(
            runtime_id,
            RelativeWorkingDirectory::new(".")?,
            ValidatedCommand::shell(script)?,
            ExecutionEnvironment::new(BTreeMap::new(), vec![])?,
            limits(timeout_ms),
        )
    }

    fn success_script() -> &'static str {
        "printf 'local-ok\\n'"
    }
    fn failure_script() -> &'static str {
        "printf 'failed\\n'; exit 7"
    }
    fn sleep_script() -> &'static str {
        "sleep 5"
    }
    fn background_script() -> &'static str {
        "sleep 5 &"
    }

    fn descendant_script() -> &'static str {
        "sleep 30 & child=$!; printf '%s\\n' \"$child\"; wait \"$child\""
    }

    #[cfg(windows)]
    async fn process_exists(pid: u32) -> bool {
        let Some(program) = janus_infrastructure::shell::bash_program() else {
            return false;
        };
        // Git Bash reports MSYS PIDs, which are not the same namespace as
        // Windows process IDs. Use Bash's built-in probe in that namespace.
        Command::new(program)
            .args(["--login", "-c", &format!("kill -0 {pid}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .is_ok_and(|status| status.success())
    }

    #[cfg(not(windows))]
    async fn process_exists(pid: u32) -> bool {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .is_ok_and(|status| status.success())
    }
}
