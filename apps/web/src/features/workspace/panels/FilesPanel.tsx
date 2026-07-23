import { Folder } from "lucide-react";
import { useWorkspaceTree } from "../hooks";
import { FileTree } from "./FileTree";

interface FilesPanelProps {
  projectId: string;
  onFileSelect?: ((path: string) => void) | undefined;
}

export function FilesPanel({ projectId, onFileSelect }: FilesPanelProps) {
  const { data, isLoading, error } = useWorkspaceTree(projectId);

  return (
    <div className="flex h-full flex-col">
      {/* Header */}
      <div className="flex h-11 shrink-0 items-center gap-2 border-b border-border px-4">
        <Folder className="h-4 w-4 shrink-0 text-muted-foreground" />
        <h2 className="text-sm font-semibold">Files</h2>
      </div>

      {/* Content */}
      <div className="flex-1 overflow-hidden">
        {isLoading && (
          <div className="flex h-full items-center justify-center">
            <div className="text-sm text-muted-foreground">
              Loading files...
            </div>
          </div>
        )}

        {error && (
          <div className="flex h-full items-center justify-center p-4">
            <div className="text-center">
              <p className="text-sm text-destructive">Failed to load files</p>
              <p className="mt-1 text-xs text-muted-foreground">
                {error instanceof Error ? error.message : "Unknown error"}
              </p>
            </div>
          </div>
        )}

        {data && data.entries.length === 0 && (
          <div className="flex h-full items-center justify-center p-4">
            <div className="text-center text-sm text-muted-foreground">
              No files found
            </div>
          </div>
        )}

        {data && data.entries.length > 0 && (
          <FileTree
            projectId={projectId}
            entries={data.entries}
            onFileSelect={onFileSelect}
          />
        )}
      </div>
    </div>
  );
}
