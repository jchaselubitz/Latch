import { createRelayServer } from './server.ts';

function required(name: string): string {
  const value = process.env[name]?.trim();
  if (!value) throw new Error(`${name} is required`);
  return value;
}

const issuer = required('ADMISSION_ISSUER');
const controlPlane = required('CONTROL_PLANE_URL').replace(/\/$/, '');
const serviceToken = required('RELAY_SERVICE_TOKEN');
const invalidationSecret = required('RELAY_INVALIDATION_SECRET');
const kid = required('ADMISSION_KEY_ID');
const publicKey = required('ADMISSION_PUBLIC_KEY_PEM').replace(/\\n/g, '\n');
const port = Number(process.env.PORT ?? '8080');

// Key rotation keeps one previous verification key for the bounded overlap
// during which admissions signed before the rotation are still unexpired.
// Both values must be set together; drop them again once the overlap ends.
const previousKid = process.env.ADMISSION_PREVIOUS_KEY_ID?.trim();
const previousPublicKey = process.env.ADMISSION_PREVIOUS_PUBLIC_KEY_PEM?.replace(/\\n/g, '\n').trim();
if (Boolean(previousKid) !== Boolean(previousPublicKey)) {
  throw new Error('ADMISSION_PREVIOUS_KEY_ID and ADMISSION_PREVIOUS_PUBLIC_KEY_PEM must be set together');
}
if (previousKid === kid) throw new Error('ADMISSION_PREVIOUS_KEY_ID must differ from ADMISSION_KEY_ID');
const publicKeys = new Map([[kid, publicKey]]);
if (previousKid && previousPublicKey) publicKeys.set(previousKid, previousPublicKey);

const relay = createRelayServer({
  host: process.env.HOST ?? '0.0.0.0',
  port,
  issuer,
  publicKeys,
  invalidationSecret,
  redeem: async (claim, attemptId) => {
    const response = await fetch(`${controlPlane}/private/v1/relay/redemptions`, {
      method: 'POST',
      headers: { authorization: `Bearer ${serviceToken}`, 'content-type': 'application/json' },
      body: JSON.stringify({ ticketId: claim.jti, attemptId }),
    });
    if (!response.ok) throw new Error('ticket redemption refused');
    return await response.json() as { leaseId: string; expiresAt: number };
  },
  ...(process.env.TLS_CERT_PATH && process.env.TLS_KEY_PATH
    ? { tls: { certPath: process.env.TLS_CERT_PATH, keyPath: process.env.TLS_KEY_PATH } }
    : {}),
});

await relay.listen();
const stop = () => { void relay.drain().finally(() => process.exit(0)); };
process.on('SIGTERM', stop);
process.on('SIGINT', stop);
