'use client';

import { useState } from 'react';

import { LatchSendError, type LatchClient } from '@latch/client';

import { composerBlockReason } from './interaction.ts';
import { useSessionCapabilities } from './useCapabilities.ts';

export type ComposerProps = {
  client: LatchClient;
  sessionId: string;
  onSent?: () => void;
  onError?: (error: unknown) => void;
};

export function Composer({ client, sessionId, onSent, onError }: ComposerProps) {
  const { capabilities, refresh } = useSessionCapabilities({ client, sessionId });
  const [text, setText] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const blocked = composerBlockReason({ capabilities });
  const trimmed = text.trim();
  const submitDisabled = blocked != null || submitting || trimmed.length === 0;

  async function submit() {
    if (submitDisabled) {
      return;
    }
    setSubmitting(true);
    setError(null);
    try {
      await client.send({ sessionId, message: trimmed });
      setText('');
      onSent?.();
      await refresh();
    } catch (caught) {
      const reason = caught instanceof LatchSendError ? caught.reason : 'send failed';
      setError(reason);
      onError?.(caught);
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <form
      data-latch-composer=""
      onSubmit={event => {
        event.preventDefault();
        void submit();
      }}
    >
      <textarea
        value={text}
        onChange={event => setText(event.target.value)}
        disabled={blocked != null || submitting}
        aria-disabled={blocked != null}
        aria-label="Message"
      />
      {blocked ? <p data-latch-composer-reason="">{blocked}</p> : null}
      {error ? <p data-latch-composer-error="">{error}</p> : null}
      <button type="submit" disabled={submitDisabled}>
        Send
      </button>
    </form>
  );
}
