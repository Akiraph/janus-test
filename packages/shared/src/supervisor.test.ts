import { describe, expect, test } from "bun:test";
import {
  startSupervisorRunRequestSchema,
  supervisorRunTaskMaxChars,
} from "./supervisor";

describe("startSupervisorRunRequestSchema", () => {
  test("rejects removed permission mode inputs", () => {
    const parsed = startSupervisorRunRequestSchema.safeParse({
      projectId: "project-1",
      task: "Update README",
      permissionMode: "guarded",
    });

    expect(parsed.success).toBe(false);
  });

  test("defaults the worker image without a permission mode", () => {
    expect(
      startSupervisorRunRequestSchema.parse({
        projectId: "project-1",
        task: "Update README",
      }),
    ).toEqual({
      projectId: "project-1",
      task: "Update README",
      image: "janus-cli-worker:dev",
      deliveryIntent: "queue",
    });
  });

  test("accepts bounded attachments and best-of-n options", () => {
    expect(
      startSupervisorRunRequestSchema.parse({
        projectId: "project-1",
        task: "Review the attached notes",
        attachments: [
          {
            name: "notes.txt",
            mediaType: "text/plain",
            sizeBytes: 11,
            contentBase64: "aGVsbG8gd29ybGQ=",
          },
        ],
        bestOfN: {
          candidateCount: 3,
        },
      }),
    ).toMatchObject({
      attachments: [
        {
          name: "notes.txt",
          mediaType: "text/plain",
          sizeBytes: 11,
        },
      ],
      bestOfN: {
        candidateCount: 3,
      },
    });
  });

  test("rejects invalid attachment payloads", () => {
    const parsed = startSupervisorRunRequestSchema.safeParse({
      projectId: "project-1",
      task: "Review this",
      attachments: [
        {
          name: "notes.txt",
          sizeBytes: 4,
          contentBase64: "../not-base64",
        },
      ],
    });

    expect(parsed.success).toBe(false);
  });

  test("accepts interrupt delivery and selected discussion models", () => {
    expect(
      startSupervisorRunRequestSchema.parse({
        projectId: "project-1",
        sessionId: "session-1",
        task: "Review the design",
        deliveryIntent: "interrupt",
        discussionModelIds: ["provider-a:default", "provider-b:reviewer"],
      }),
    ).toMatchObject({
      deliveryIntent: "interrupt",
      discussionModelIds: ["provider-a:default", "provider-b:reviewer"],
    });
  });

  test("rejects duplicate selected discussion models", () => {
    const parsed = startSupervisorRunRequestSchema.safeParse({
      projectId: "project-1",
      task: "Review the design",
      discussionModelIds: ["provider-a:default", "provider-a:default"],
    });

    expect(parsed.success).toBe(false);
  });

  test("bounds task text before persistence or model execution", () => {
    expect(
      startSupervisorRunRequestSchema.safeParse({
        projectId: "project-1",
        task: "x".repeat(supervisorRunTaskMaxChars),
      }).success,
    ).toBe(true);

    expect(
      startSupervisorRunRequestSchema.safeParse({
        projectId: "project-1",
        task: "x".repeat(supervisorRunTaskMaxChars + 1),
      }).success,
    ).toBe(false);
  });
});
