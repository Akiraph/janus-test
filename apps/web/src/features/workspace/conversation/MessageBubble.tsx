import { MarkdownOutput } from "./MarkdownOutput";

interface MessageBubbleProps {
  sender: "user" | "assistant";
  content: string;
  timestamp?: string;
}

export function MessageBubble({ sender, content }: MessageBubbleProps) {
  const isUser = sender === "user";

  return (
    <div className={`flex ${isUser ? "justify-end" : "justify-start"}`}>
      <div
        className={`flex max-w-[70%] flex-col ${isUser ? "items-end" : "items-start"}`}
      >
        <div
          className={`rounded-lg px-4 py-3 ${
            isUser ? "bg-muted text-foreground" : "bg-card text-foreground"
          }`}
        >
          {isUser ? (
            <p className="text-sm whitespace-pre-wrap">{content}</p>
          ) : (
            <MarkdownOutput text={content} />
          )}
        </div>
      </div>
    </div>
  );
}
