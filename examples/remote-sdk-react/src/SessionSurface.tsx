import type { LatchClient } from '@latch/client';
import { AwaitingInputPrompt, Composer, useTranscript } from '@latch/chat-react';
import { LatchTerminal } from '@latch/terminal-react';

import { TranscriptTimeline } from './TranscriptTimeline.tsx';

export function SessionSurface({
  client,
  sessionId,
  chatEnabled,
  sendEnabled
}: {
  client: LatchClient;
  sessionId: string;
  chatEnabled: boolean;
  sendEnabled: boolean;
}) {
  return (
    <section className="surface">
      <div className="terminal">
        <LatchTerminal client={client} sessionId={sessionId} />
      </div>
      {chatEnabled ? (
        <ChatSurface client={client} sessionId={sessionId} sendEnabled={sendEnabled} />
      ) : (
        <p>The connected gateway does not offer events. The terminal remains available.</p>
      )}
    </section>
  );
}

export default SessionSurface;

function ChatSurface({
  client,
  sessionId,
  sendEnabled
}: {
  client: LatchClient;
  sessionId: string;
  sendEnabled: boolean;
}) {
  const transcript = useTranscript({ client, sessionId });
  const pending = transcript.items.find(
    item => item.kind === 'awaiting_input' && !item.resolved && item.requestId === transcript.pendingRequestId
  );

  return (
    <div className="chat">
      <p>Transcript connection: {transcript.connection}</p>
      <TranscriptTimeline items={transcript.items} />
      {sendEnabled && pending?.kind === 'awaiting_input' ? (
        <AwaitingInputPrompt
          client={client}
          sessionId={sessionId}
          requestId={pending.requestId}
          prompt={pending.prompt}
          choices={pending.choices}
          activeRequestId={transcript.pendingRequestId}
        />
      ) : null}
      {sendEnabled ? <Composer client={client} sessionId={sessionId} /> : null}
    </div>
  );
}
