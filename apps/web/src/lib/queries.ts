import { createQuery } from "@tanstack/solid-query";
import { getBootstrap, getMe, getModels, getProviders, getSystemInfo } from "./api";

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
export function useModels() {
  return createQuery(() => ({ queryKey: ["models"], queryFn: getModels }));
}
