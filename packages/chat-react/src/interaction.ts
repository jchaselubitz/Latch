import type { SessionCapabilities } from '@latch/client';

export function composerBlockReason({
  capabilities
}: {
  capabilities: SessionCapabilities | null;
}): string | null {
  if (!capabilities) {
    return 'checking whether this session can accept a message';
  }
  if (!capabilities.canSend.ok) {
    return capabilities.canSend.reason ?? 'this session cannot accept a message right now';
  }
  if (!capabilities.sendMessage) {
    return 'free-text send is not available on the current screen';
  }
  return null;
}

export function promptBlockReason({
  capabilities,
  requestId,
  activeRequestId
}: {
  capabilities: SessionCapabilities | null;
  requestId: string;
  activeRequestId?: string | null;
}): string | null {
  if (activeRequestId != null && activeRequestId !== requestId) {
    return 'this prompt is no longer the visible request';
  }
  if (!capabilities) {
    return 'checking whether this prompt can be answered';
  }
  if (!capabilities.resolve) {
    return (
      capabilities.canSend.reason ?? 'this prompt cannot be answered from chat'
    );
  }
  return null;
}
