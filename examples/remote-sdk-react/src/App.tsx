import { FormEvent, lazy, Suspense, useEffect, useMemo, useState } from 'react';

import {
  createLatchClient,
  supportsGatewayEndpoint,
  type GatewayCapabilities,
  type LatchClient,
  type SessionSummary
} from '@latch/client';

const SessionSurface = lazy(() => import('./SessionSurface.tsx'));

type Connection = {
  url: string;
  token: string;
};

const storageKey = 'latch.remote-sdk-example.connection.v1';

export function App() {
  const [draft, setDraft] = useState<Connection>(() => loadConnection());
  const [connection, setConnection] = useState<Connection | null>(null);
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [gateway, setGateway] = useState<GatewayCapabilities | null>(null);
  const [selectedSessionId, setSelectedSessionId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const client = useMemo(
    () =>
      connection
        ? createLatchClient({ url: connection.url.trim(), token: connection.token.trim() })
        : null,
    [connection]
  );

  useEffect(() => {
    if (!client) {
      return;
    }
    let cancelled = false;
    setError(null);
    void Promise.all([client.listSessions(), client.gatewayCapabilities()])
      .then(([list, capabilities]) => {
        if (cancelled) {
          return;
        }
        setSessions(list.sessions);
        setGateway(capabilities);
        setSelectedSessionId(current => current ?? list.sessions[0]?.id ?? null);
      })
      .catch(caught => {
        if (!cancelled) {
          setError(caught instanceof Error ? caught.message : 'could not connect to latch serve');
        }
      });
    return () => {
      cancelled = true;
    };
  }, [client]);

  function connect(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const next = { url: draft.url.trim(), token: draft.token.trim() };
    if (!next.url || !next.token) {
      setError('Gateway URL and bearer token are required.');
      return;
    }
    localStorage.setItem(storageKey, JSON.stringify(next));
    setSelectedSessionId(null);
    setSessions([]);
    setGateway(null);
    setConnection(next);
  }

  const chatEnabled = gateway
    ? supportsGatewayEndpoint({ capabilities: gateway, endpoint: 'events' })
    : false;
  const sendEnabled = gateway
    ? supportsGatewayEndpoint({ capabilities: gateway, endpoint: 'send' })
    : false;

  return (
    <main>
      <header>
        <h1>Latch Remote SDK</h1>
        <p>Published-package composition example: sessions, terminal, chat, and interaction.</p>
      </header>
      <form className="connection" onSubmit={connect}>
        <label>
          Gateway URL
          <input
            value={draft.url}
            onChange={event => setDraft(current => ({ ...current, url: event.target.value }))}
            placeholder="http://127.0.0.1:4610"
            inputMode="url"
          />
        </label>
        <label>
          Bearer token
          <input
            value={draft.token}
            onChange={event => setDraft(current => ({ ...current, token: event.target.value }))}
            type="password"
            autoComplete="off"
          />
        </label>
        <button type="submit">Connect</button>
      </form>
      {error ? <p role="alert">{error}</p> : null}
      {client && gateway ? (
        <section className="sessions" aria-label="Sessions">
          <label>
            Session
            <select
              value={selectedSessionId ?? ''}
              onChange={event => setSelectedSessionId(event.target.value || null)}
            >
              <option value="">Choose a session</option>
              {sessions.map(session => (
                <option key={session.id} value={session.id}>
                  {session.title ?? session.name} ({session.state})
                </option>
              ))}
            </select>
          </label>
          <p>
            Gateway {gateway.productVersion} · chat {chatEnabled ? 'available' : 'unavailable'} ·
            interaction {sendEnabled ? 'available' : 'unavailable'}
          </p>
        </section>
      ) : null}
      {client && selectedSessionId ? (
        <Suspense fallback={<p>Opening session…</p>}>
          <SessionSurface
            key={selectedSessionId}
            client={client}
            sessionId={selectedSessionId}
            chatEnabled={chatEnabled}
            sendEnabled={sendEnabled}
          />
        </Suspense>
      ) : null}
    </main>
  );
}

function loadConnection(): Connection {
  try {
    const value = localStorage.getItem(storageKey);
    if (value) {
      const parsed = JSON.parse(value) as Partial<Connection>;
      if (typeof parsed.url === 'string' && typeof parsed.token === 'string') {
        return { url: parsed.url, token: parsed.token };
      }
    }
  } catch {
    // Configuration is a convenience only; a malformed saved value is ignored.
  }
  return { url: 'http://127.0.0.1:4610', token: '' };
}
