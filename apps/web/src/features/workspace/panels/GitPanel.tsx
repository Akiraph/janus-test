import { SourceControlPanel } from "./SourceControlPanel";

interface GitPanelProps {
  projectId: string;
  onFileSelect?: ((path: string) => void) | undefined;
}

export function GitPanel({ projectId, onFileSelect }: GitPanelProps) {
  return (
    <SourceControlPanel projectId={projectId} onFileSelect={onFileSelect} />
  );
}
