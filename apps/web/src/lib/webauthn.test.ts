import { describe, expect, it } from "bun:test";
import { authenticationOptions, registrationOptions } from "./webauthn";

describe("WebAuthn option normalization", () => {
  it("unwraps the backend registration publicKey envelope", () => {
    const options = registrationOptions({
      publicKey: {
        challenge: "AQID",
        user: { id: "BAUG", name: "owner@janus.local", displayName: "Owner" },
      },
    });

    expect(Array.from(options.challenge as Uint8Array)).toEqual([1, 2, 3]);
    expect(Array.from(options.user.id as Uint8Array)).toEqual([4, 5, 6]);
  });

  it("unwraps the backend authentication publicKey envelope", () => {
    const options = authenticationOptions({
      publicKey: {
        challenge: "AQID",
        allowCredentials: [{ id: "BAUG", type: "public-key" }],
      },
    });

    expect(Array.from(options.challenge as Uint8Array)).toEqual([1, 2, 3]);
    expect(Array.from(options.allowCredentials?.[0]?.id as Uint8Array)).toEqual([4, 5, 6]);
  });
});
