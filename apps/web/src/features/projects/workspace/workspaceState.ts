import { createEffect, createMemo, createSignal } from "solid-js";
import { createStore, produce, reconcile } from "solid-js/store";
import type { FileMetaView } from "../../../lib/api";

export interface FileDocument {
  id: string;
  kind: "file";
  path: string;
  draft: string;
  saved: string;
  meta: FileMetaView | null;
  loadError: string;
  loading: boolean;
}

export interface SessionDocument {
  id: string;
  kind: "session";
  sessionId: string;
  title: string;
  subView: "main" | "diff" | "async";
}

export type WorkspaceDocument = FileDocument | SessionDocument;
export type WorkspaceActivity = "explorer" | "sessions" | "scm" | "terminal";

export function createWorkspaceState(compact: () => boolean) {
  const [activity, setActivity] = createSignal<WorkspaceActivity>("sessions");
  const [navigationOpen, setNavigationOpen] = createSignal(true);
  const [documents, setDocuments] = createStore<WorkspaceDocument[]>([]);
  const [activeDocumentId, setActiveDocumentId] = createSignal<string | null>(null);
  let documentSequence = 0;

  const activeDocument = createMemo(() => {
    const id = activeDocumentId();
    return id ? documents.find((document) => document.id === id) : undefined;
  });
  const activeFilePath = createMemo(() => {
    const document = activeDocument();
    return document?.kind === "file" ? document.path : null;
  });
  const activeSessionId = createMemo(() => {
    const document = activeDocument();
    return document?.kind === "session" ? document.sessionId : null;
  });

  function nextDocumentId() {
    documentSequence += 1;
    return `document-${documentSequence}`;
  }

  function selectActivity(next: WorkspaceActivity) {
    if (activity() === next) {
      if (compact()) {
        setNavigationOpen(!activeDocument());
        return;
      }
      setNavigationOpen((open) => !open);
      return;
    }
    setActivity(next);
    setNavigationOpen(true);
  }

  function openNavigation() {
    setNavigationOpen(true);
  }

  function activateDocument(id: string) {
    if (documents.some((document) => document.id === id)) {
      setActiveDocumentId(id);
      if (compact()) setNavigationOpen(false);
    }
  }

  function openFile(path: string) {
    const existing = documents.find(
      (document) => document.kind === "file" && document.path === path,
    );
    if (existing) {
      activateDocument(existing.id);
      return;
    }
    const id = nextDocumentId();
    setDocuments(documents.length, {
      id,
      kind: "file",
      path,
      draft: "",
      saved: "",
      meta: null,
      loadError: "",
      loading: false,
    });
    setActiveDocumentId(id);
    if (compact()) setNavigationOpen(false);
  }

  function openSession(sessionId: string, title?: string | null) {
    const existing = documents.find(
      (document) => document.kind === "session" && document.sessionId === sessionId,
    );
    if (existing) {
      setActivity("sessions");
      activateDocument(existing.id);
      return;
    }
    const id = nextDocumentId();
    setDocuments(documents.length, {
      id,
      kind: "session",
      sessionId,
      title: title?.trim() || "New session",
      subView: "main",
    });
    setActiveDocumentId(id);
    setActivity("sessions");
    setNavigationOpen(!compact());
  }

  function closeSession(sessionId: string) {
    const removed = documents.find(
      (document) => document.kind === "session" && document.sessionId === sessionId,
    );
    if (!removed) return;
    const remaining = documents.filter((document) => document.id !== removed.id);
    setDocuments(reconcile(remaining));
    if (activeDocumentId() === removed.id) {
      setActiveDocumentId(remaining.at(-1)?.id ?? null);
    }
    if (compact() && remaining.length === 0) setNavigationOpen(true);
  }

  function closeDocument(id: string) {
    const index = documents.findIndex((document) => document.id === id);
    if (index < 0) return;
    const neighbor = documents[index + 1] ?? documents[index - 1] ?? null;
    const remaining = documents.filter((document) => document.id !== id);
    setDocuments(reconcile(remaining));
    if (activeDocumentId() === id) setActiveDocumentId(neighbor?.id ?? null);
    if (compact() && remaining.length === 0) setNavigationOpen(true);
  }

  function updateFile(id: string, update: (document: FileDocument) => void) {
    setDocuments(
      produce((items) => {
        const document = items.find((item) => item.id === id);
        if (document?.kind === "file") update(document);
      }),
    );
  }

  function updateSession(id: string, update: (document: SessionDocument) => void) {
    setDocuments(
      produce((items) => {
        const document = items.find((item) => item.id === id);
        if (document?.kind === "session") update(document);
      }),
    );
  }

  function reset() {
    setDocuments(reconcile([]));
    setActiveDocumentId(null);
    setActivity("sessions");
    setNavigationOpen(true);
    documentSequence = 0;
  }

  let previousCompact = compact();
  createEffect(() => {
    const nextCompact = compact();
    if (nextCompact === previousCompact) return;
    setNavigationOpen(nextCompact ? !activeDocument() : true);
    previousCompact = nextCompact;
  });

  return {
    activity,
    navigationOpen,
    documents,
    activeDocumentId,
    activeDocument,
    activeFilePath,
    activeSessionId,
    selectActivity,
    openNavigation,
    activateDocument,
    openFile,
    openSession,
    closeSession,
    closeDocument,
    updateFile,
    updateSession,
    reset,
  };
}

export type WorkspaceState = ReturnType<typeof createWorkspaceState>;
