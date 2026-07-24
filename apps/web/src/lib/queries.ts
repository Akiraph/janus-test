import { createQuery } from "@tanstack/solid-query";
import {
  getBootstrap,
  getMe,
  getProject,
  getProviders,
  getSystemInfo,
  gitLog,
  gitStatus,
  listFileTree,
  listGithubCredentials,
  listProjects,
} from "./api";

export function useBootstrap() {
  return createQuery(() => ({
    queryKey: ["bootstrap"],
    queryFn: getBootstrap,
  }));
}

export function useSystemInfo() {
  return createQuery(() => ({
    queryKey: ["system-info"],
    queryFn: getSystemInfo,
  }));
}

export function useMe() {
  return createQuery(() => ({ queryKey: ["me"], queryFn: getMe, retry: false }));
}

export function useProviders() {
  return createQuery(() => ({ queryKey: ["model-providers"], queryFn: getProviders }));
}

export function useProjects() {
  return createQuery(() => ({
    queryKey: ["projects"],
    queryFn: () => listProjects(),
  }));
}

export function useProject(id: () => string | undefined) {
  return createQuery(() => {
    const projectId = id();
    return {
      queryKey: ["project", projectId],
      queryFn: () => getProject(projectId as string),
      enabled: Boolean(projectId),
    };
  });
}

export function useGithubCredentials() {
  return createQuery(() => ({
    queryKey: ["github-credentials"],
    queryFn: listGithubCredentials,
  }));
}

export function useFileTree(projectId: () => string | undefined, path: () => string = () => "") {
  return createQuery(() => {
    const id = projectId();
    const treePath = path();
    return {
      queryKey: ["file-tree", id, treePath],
      queryFn: () => listFileTree(id as string, treePath || undefined),
      enabled: Boolean(id),
    };
  });
}

export function useGitStatus(projectId: () => string | undefined) {
  return createQuery(() => {
    const id = projectId();
    return {
      queryKey: ["git-status", id],
      queryFn: () => gitStatus(id as string),
      enabled: Boolean(id),
    };
  });
}

export function useGitLog(projectId: () => string | undefined, limit = 30) {
  return createQuery(() => {
    const id = projectId();
    return {
      queryKey: ["git-log", id, limit],
      queryFn: () => gitLog(id as string, limit),
      enabled: Boolean(id),
    };
  });
}
