function decode(value: string): Uint8Array {
  const normalized = value
    .replace(/-/g, "+")
    .replace(/_/g, "/")
    .padEnd(Math.ceil(value.length / 4) * 4, "=");
  const binary = atob(normalized);
  return Uint8Array.from(binary, (character) => character.charCodeAt(0));
}

function encode(value: ArrayBuffer): string {
  const binary = String.fromCharCode(...new Uint8Array(value));
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/g, "");
}

function decodeFields(value: unknown, fields: string[]): Record<string, unknown> {
  const source = { ...(value as Record<string, unknown>) };
  for (const field of fields)
    if (typeof source[field] === "string") source[field] = decode(source[field] as string);
  return source;
}

export function registrationOptions(value: unknown): PublicKeyCredentialCreationOptions {
  const source = { ...(value as Record<string, unknown>) };
  source.challenge = decode(source.challenge as string);
  source.user = decodeFields(source.user, ["id"]);
  if (Array.isArray(source.excludeCredentials))
    source.excludeCredentials = source.excludeCredentials.map((item) => decodeFields(item, ["id"]));
  return source as unknown as PublicKeyCredentialCreationOptions;
}

export function authenticationOptions(value: unknown): PublicKeyCredentialRequestOptions {
  const source = { ...(value as Record<string, unknown>) };
  source.challenge = decode(source.challenge as string);
  if (Array.isArray(source.allowCredentials))
    source.allowCredentials = source.allowCredentials.map((item) => decodeFields(item, ["id"]));
  return source as unknown as PublicKeyCredentialRequestOptions;
}

export function credentialPayload(credential: PublicKeyCredential): Record<string, unknown> {
  const response = credential.response as
    | AuthenticatorAttestationResponse
    | AuthenticatorAssertionResponse;
  const payload: Record<string, unknown> = {
    id: credential.id,
    rawId: encode(credential.rawId),
    type: credential.type,
    response: {
      clientDataJSON: encode(response.clientDataJSON),
    },
  };
  if ("attestationObject" in response) {
    (payload.response as Record<string, unknown>).attestationObject = encode(
      response.attestationObject,
    );
  } else {
    const assertion = response as AuthenticatorAssertionResponse;
    (payload.response as Record<string, unknown>).authenticatorData = encode(
      assertion.authenticatorData,
    );
    (payload.response as Record<string, unknown>).signature = encode(assertion.signature);
    if (assertion.userHandle)
      (payload.response as Record<string, unknown>).userHandle = encode(assertion.userHandle);
  }
  return payload;
}
