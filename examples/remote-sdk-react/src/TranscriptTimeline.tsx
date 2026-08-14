import type { TranscriptItem } from '@latch/chat-react';

export function TranscriptTimeline({ items }: { items: TranscriptItem[] }) {
  return (
    <ol className="timeline" aria-live="polite">
      {items.map(item => (
        <li key={item.id}>
          {item.kind === 'tool' ? (
            <details open={!item.finished}>
              <summary>{item.tool}</summary>
              <pre>{JSON.stringify(item.input, null, 2)}</pre>
              {item.finished ? <pre>{JSON.stringify(item.output, null, 2)}</pre> : null}
            </details>
          ) : item.kind === 'awaiting_input' ? (
            <p>{item.resolved ? 'Answered: ' : 'Awaiting input: '}{item.prompt}</p>
          ) : (
            <p><strong>{item.kind}</strong>: {item.text}</p>
          )}
        </li>
      ))}
    </ol>
  );
}
