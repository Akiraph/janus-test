import type { SupervisorAskRequestRecord } from "@janus/shared";
import { Loader2, Send } from "lucide-react";
import { useState } from "react";
import { Button } from "../../../components/ui/button";
import { StatusDot } from "../../../components/ui/status-dot";
import { Textarea } from "../../../components/ui/textarea";

export function PendingAskCard({
  ask,
  submitting,
  onSubmit,
}: {
  readonly ask: SupervisorAskRequestRecord;
  readonly submitting: boolean;
  readonly onSubmit: (answer: string) => void;
}) {
  const [answer, setAnswer] = useState("");
  const trimmed = answer.trim();

  return (
    <form
      className="rounded-md border border-border-accent bg-background p-3 shadow-card"
      onSubmit={(event) => {
        event.preventDefault();
        if (trimmed.length > 0 && !submitting) {
          onSubmit(trimmed);
          setAnswer("");
        }
      }}
    >
      <div className="flex items-start gap-2">
        <StatusDot tone="live" pulse className="mt-1.5 shrink-0" />
        <div className="min-w-0 flex-1">
          <p className="text-sm font-medium text-foreground">{ask.question}</p>
          {ask.context === undefined ? null : (
            <p className="mt-1 text-xs text-muted-foreground">{ask.context}</p>
          )}
          {ask.options === undefined ? null : (
            <div className="mt-2 flex flex-wrap gap-1.5">
              {ask.options.map((option) => (
                <button
                  key={option}
                  type="button"
                  onClick={() => setAnswer(option)}
                  className="rounded-sm border border-border bg-muted px-2 py-1 text-xs text-foreground transition-colors hover:border-border-accent"
                >
                  {option}
                </button>
              ))}
            </div>
          )}
          <div className="mt-2 flex items-end gap-2">
            <Textarea
              value={answer}
              onChange={(event) => setAnswer(event.target.value)}
              autoResize
              disabled={submitting}
              aria-label="Answer question"
              className="min-h-10 max-h-32 flex-1 bg-background"
            />
            <Button
              type="submit"
              size="sm"
              disabled={submitting || trimmed.length === 0}
            >
              {submitting ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
              ) : (
                <Send className="h-3.5 w-3.5" />
              )}
              Send
            </Button>
          </div>
        </div>
      </div>
    </form>
  );
}
