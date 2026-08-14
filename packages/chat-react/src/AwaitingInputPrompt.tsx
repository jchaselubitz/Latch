'use client';

import { useState } from 'react';

import { LatchSendError, type LatchClient } from '@latch/client';

import { promptBlockReason } from './interaction.ts';
import { useSessionCapabilities } from './useCapabilities.ts';

export type AwaitingInputPromptProps = {
  client: LatchClient;
  sessionId: string;
  requestId: string;
  prompt: string;
  choices?: string[];
  // When set, a widget whose requestId is not this value cannot submit —
  // it is bound to the prompt it was rendered for, not a later one.
  activeRequestId?: string | null;
  onResolved?: (choice: string) => void;
  onError?: (error: unknown) => void;
};

export function AwaitingInputPrompt({
  client,
  sessionId,
  requestId,
  prompt,
  choices = [],
  activeRequestId,
  onResolved,
  onError
}: AwaitingInputPromptProps) {
  const { capabilities, refresh } = useSessionCapabilities({ client, sessionId });
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const blocked = promptBlockReason({ capabilities, requestId, activeRequestId });
  const disabled = blocked != null || submitting;

  async function answer({ choice }: { choice: string }) {
    if (disabled) {
      return;
    }
    setSubmitting(true);
    setError(null);
    try {
      await client.send({ sessionId, resolve: { requestId, choice } });
      onResolved?.(choice);
      await refresh();
    } catch (caught) {
      const reason = caught instanceof LatchSendError ? caught.reason : 'resolve failed';
      setError(reason);
      onError?.(caught);
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <fieldset data-latch-awaiting-input="" data-request-id={requestId} disabled={disabled}>
      <legend>{prompt}</legend>
      {blocked ? <p data-latch-awaiting-reason="">{blocked}</p> : null}
      {error ? <p data-latch-awaiting-error="">{error}</p> : null}
      <div>
        {choices.map(choice => (
          <button
            key={choice}
            type="button"
            onClick={() => {
              void answer({ choice });
            }}
          >
            {choice}
          </button>
        ))}
      </div>
    </fieldset>
  );
}
