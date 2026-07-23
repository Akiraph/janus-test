import { Trash2 } from "lucide-react";
import { useState } from "react";
import { useNavigate } from "react-router-dom";
import { Button } from "../../../components/ui/button";
import { Dialog } from "../../../components/ui/dialog";
import { useDeleteProject } from "../hooks/useDeleteProject";

export interface ProjectSettingsDialogProps {
  readonly projectId: string;
  readonly open: boolean;
  readonly onOpenChange: (open: boolean) => void;
}

/**
 * ProjectSettingsDialog — Project-specific settings
 * Currently only includes delete project functionality
 */
export function ProjectSettingsDialog({
  projectId,
  open,
  onOpenChange,
}: ProjectSettingsDialogProps) {
  const navigate = useNavigate();
  const deleteProjectMutation = useDeleteProject();
  const [confirmDelete, setConfirmDelete] = useState(false);

  const handleDeleteProject = async () => {
    try {
      await deleteProjectMutation.mutateAsync(projectId);
      onOpenChange(false);
      navigate("/");
    } catch (error) {
      console.error("Failed to delete project:", error);
    }
  };

  return (
    <Dialog
      open={open}
      onOpenChange={(newOpen) => {
        onOpenChange(newOpen);
        if (!newOpen) {
          setConfirmDelete(false);
        }
      }}
      title="Project Settings"
      className="max-w-md"
    >
      <div className="space-y-6">
        {/* Danger Zone */}
        <div className="rounded-lg border border-destructive/30 bg-destructive/5 p-4">
          <h3 className="text-sm font-semibold text-destructive mb-2">
            Danger Zone
          </h3>
          <p className="text-xs text-muted-foreground mb-4">
            Delete this project and all its sessions. This action cannot be
            undone.
          </p>

          {!confirmDelete ? (
            <Button
              variant="outline"
              size="sm"
              onClick={() => setConfirmDelete(true)}
              className="gap-2 border-destructive/50 text-destructive hover:bg-destructive/10"
            >
              <Trash2 className="h-4 w-4" />
              Delete Project
            </Button>
          ) : (
            <div className="space-y-3">
              <p className="text-sm font-medium text-destructive">
                Are you sure? This cannot be undone.
              </p>
              <div className="flex gap-2">
                <Button
                  variant="destructive"
                  size="sm"
                  onClick={handleDeleteProject}
                  disabled={deleteProjectMutation.isPending}
                >
                  {deleteProjectMutation.isPending
                    ? "Deleting..."
                    : "Yes, Delete"}
                </Button>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => setConfirmDelete(false)}
                  disabled={deleteProjectMutation.isPending}
                >
                  Cancel
                </Button>
              </div>
              {deleteProjectMutation.isError && (
                <p className="text-xs text-destructive">
                  Failed to delete project. Please try again.
                </p>
              )}
            </div>
          )}
        </div>
      </div>
    </Dialog>
  );
}
