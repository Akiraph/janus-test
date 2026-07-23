import type { ProjectSummary } from "@janus/shared";
import { ArrowRight, Folder, Plus } from "lucide-react";
import { useState } from "react";
import { useNavigate } from "react-router-dom";
import { Button } from "../../components/ui/button";
import { Card } from "../../components/ui/card";
import { EmptyState } from "../../components/ui/empty-state";
import { IconTile } from "../../components/ui/icon-tile";
import { Skeleton } from "../../components/ui/skeleton";
import { StatusChip } from "../../components/ui/status-chip";
import { formatDistanceToNow } from "../../lib/format";
import { ConnectRepoDialog } from "./ConnectRepoDialog";
import { useProjects } from "./hooks/useProjects";

export function ProjectsSection() {
  const [showConnectDialog, setShowConnectDialog] = useState(false);
  const navigate = useNavigate();
  const { data, isLoading, isError, refetch } = useProjects();

  const projects = data?.projects ?? [];

  const handleOpenProject = (projectId: string) => {
    navigate(`/projects/${projectId}`);
  };

  return (
    <div>
      {/* Page title — quiet, no decorative hero tile */}
      <div className="mb-6">
        <h1 className="text-lg font-semibold text-foreground">Projects</h1>
        <p className="mt-0.5 text-sm text-muted-foreground">
          Pick a repository to start working.
        </p>
      </div>

      {/* Error state */}
      {isError && (
        <div className="rounded-md bg-destructive/10 p-4">
          <p className="text-sm text-destructive">
            Failed to load projects. Please try again.
          </p>
          <Button
            variant="outline"
            size="sm"
            onClick={() => refetch()}
            className="mt-3"
          >
            Retry
          </Button>
        </div>
      )}

      {/* Loading state */}
      {isLoading && (
        <div className="grid grid-cols-1 gap-4 md:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4">
          {[0, 1, 2, 3].map((i) => (
            <Card key={`skeleton-${i}`} className="p-5">
              <div className="space-y-4">
                <Skeleton className="h-12 w-12 rounded-xs" />
                <Skeleton className="h-5 w-3/4" />
                <Skeleton className="h-4 w-1/2" />
                <div className="border-t border-border pt-4">
                  <Skeleton className="h-4 w-full" />
                  <Skeleton className="mt-2 h-4 w-2/3" />
                </div>
              </div>
            </Card>
          ))}
        </div>
      )}

      {/* Empty state */}
      {!isLoading && !isError && projects.length === 0 && (
        <EmptyState
          icon={<Folder className="h-12 w-12" />}
          title="No projects yet"
          description="Connect a GitHub repository to get started."
          action={
            <Button onClick={() => setShowConnectDialog(true)}>
              Connect repository
            </Button>
          }
        />
      )}

      {/* Projects grid */}
      {!isLoading && !isError && projects.length > 0 && (
        <div className="grid grid-cols-1 gap-4 md:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4">
          {projects.map((project, index) => (
            <div
              key={project.id}
              className="animate-fade-in-up"
              style={{ animationDelay: `${Math.min(index, 7) * 45}ms` }}
            >
              <ProjectCard
                project={project}
                onOpen={() => handleOpenProject(project.id)}
              />
            </div>
          ))}

          {/* Connect repo card */}
          <div
            className="animate-fade-in-up"
            style={{ animationDelay: `${Math.min(projects.length, 7) * 45}ms` }}
          >
            <Card
              variant="dashed"
              className="flex h-full cursor-pointer flex-col items-center justify-center p-5 text-center"
              onClick={() => setShowConnectDialog(true)}
            >
              <IconTile variant="neutral" size="lg" className="mb-4 opacity-60">
                <Plus className="h-6 w-6" />
              </IconTile>
              <h3 className="text-base font-medium text-foreground">
                Connect repo
              </h3>
              <p className="mt-1 text-sm text-muted-foreground">
                Connect a Git repository to get started.
              </p>
              <Button
                variant="outline"
                size="sm"
                className="mt-4"
                onClick={(e) => {
                  e.stopPropagation();
                  setShowConnectDialog(true);
                }}
              >
                Connect repository
              </Button>
            </Card>
          </div>
        </div>
      )}

      {/* Connect dialog */}
      <ConnectRepoDialog
        open={showConnectDialog}
        onOpenChange={setShowConnectDialog}
      />
    </div>
  );
}

type ProjectCardProps = {
  project: ProjectSummary;
  onOpen: () => void;
};

function ProjectCard({ project, onOpen }: ProjectCardProps) {
  return (
    <Card className="group flex h-full flex-col p-5">
      <IconTile variant="neutral" size="lg" className="mb-4">
        <Folder className="h-6 w-6" />
      </IconTile>

      <h3 className="text-base font-medium text-foreground">
        {project.repoSlug}
      </h3>

      <div className="mt-2">
        <StatusChip
          status={project.status === "ready" ? "ready" : "failed"}
          label={project.status === "ready" ? "Ready" : "Clone failed"}
        />
      </div>

      <div className="my-4 border-t border-border" />

      <div className="space-y-1 text-sm text-muted-foreground">
        <div>Last activity: {formatDistanceToNow(project.updatedAt)}</div>
      </div>

      <Button
        variant={project.status === "ready" ? "primary" : "outline"}
        className="mt-4 w-full"
        onClick={onOpen}
        disabled={project.status !== "ready"}
      >
        {project.status === "ready" ? (
          <>
            Open project
            <ArrowRight className="ml-2 h-4 w-4 transition-transform duration-150 group-hover:translate-x-0.5" />
          </>
        ) : (
          "Clone failed"
        )}
      </Button>
    </Card>
  );
}
