/**
 * A minimal router over `node:http`.
 *
 * The control plane is a small, security-sensitive JSON surface, so it stays
 * dependency-free apart from the PostgreSQL driver: fewer third-party
 * packages on the request path is fewer supply-chain surfaces on a service
 * that mediates device authorization.
 */

import type { IncomingMessage, ServerResponse } from 'node:http';

/** Bodies are metadata only; anything larger is a client bug or an attack. */
const MAX_BODY_BYTES = 16 * 1024;

export class HttpError extends Error {
  readonly status: number;
  readonly code: string;
  readonly field: string | undefined;

  constructor(status: number, code: string, message: string, field?: string) {
    super(message);
    this.status = status;
    this.code = code;
    this.field = field;
  }
}

export interface RequestContext {
  readonly method: string;
  readonly path: string;
  readonly params: Record<string, string>;
  /** The query string, parsed. Empty for a request without one. */
  readonly query: URLSearchParams;
  readonly headers: IncomingMessage['headers'];
  /** Parsed JSON body, or `null` for bodyless requests. */
  readonly body: unknown;
}

export interface Reply {
  readonly status: number;
  readonly body: unknown;
}

export type Handler = (context: RequestContext) => Promise<Reply>;

interface Route {
  readonly method: string;
  readonly segments: readonly string[];
  readonly handler: Handler;
}

export class Router {
  #routes: Route[] = [];
  #decorate: (handler: Handler) => Handler;

  /**
   * `decorate` wraps every registered handler, which is how domain errors are
   * mapped onto the HTTP error contract in exactly one place.
   */
  constructor(decorate: (handler: Handler) => Handler = (handler) => handler) {
    this.#decorate = decorate;
  }

  add(method: string, pattern: string, handler: Handler): this {
    this.#routes.push({
      method,
      segments: pattern.split('/').filter((segment) => segment.length > 0),
      handler: this.#decorate(handler),
    });
    return this;
  }

  get(pattern: string, handler: Handler): this {
    return this.add('GET', pattern, handler);
  }

  post(pattern: string, handler: Handler): this {
    return this.add('POST', pattern, handler);
  }

  patch(pattern: string, handler: Handler): this {
    return this.add('PATCH', pattern, handler);
  }

  delete(pattern: string, handler: Handler): this {
    return this.add('DELETE', pattern, handler);
  }

  match(method: string, path: string): { handler: Handler; params: Record<string, string> } | null {
    const parts = path.split('/').filter((segment) => segment.length > 0);
    let pathExists = false;
    for (const route of this.#routes) {
      if (route.segments.length !== parts.length) {
        continue;
      }
      const params: Record<string, string> = {};
      let matched = true;
      for (const [index, segment] of route.segments.entries()) {
        const value = parts[index]!;
        if (segment.startsWith(':')) {
          params[segment.slice(1)] = decodeURIComponent(value);
        } else if (segment !== value) {
          matched = false;
          break;
        }
      }
      if (!matched) {
        continue;
      }
      pathExists = true;
      if (route.method === method) {
        return { handler: route.handler, params };
      }
    }
    if (pathExists) {
      throw new HttpError(405, 'method_not_allowed', 'method not allowed for this path');
    }
    return null;
  }
}

async function readBody(request: IncomingMessage): Promise<unknown> {
  const chunks: Buffer[] = [];
  let size = 0;
  for await (const chunk of request) {
    const buffer = chunk as Buffer;
    size += buffer.length;
    if (size > MAX_BODY_BYTES) {
      throw new HttpError(413, 'body_too_large', 'request body exceeds 16 KiB');
    }
    chunks.push(buffer);
  }
  if (size === 0) {
    return null;
  }
  const contentType = request.headers['content-type'] ?? '';
  if (!contentType.toLowerCase().startsWith('application/json')) {
    throw new HttpError(415, 'unsupported_media_type', 'content-type must be application/json');
  }
  try {
    return JSON.parse(Buffer.concat(chunks).toString('utf8'));
  } catch {
    throw new HttpError(400, 'invalid_json', 'request body is not valid JSON');
  }
}

export interface RequestLog {
  readonly method: string;
  readonly path: string;
  readonly status: number;
  readonly durationMs: number;
}

/**
 * Builds the `node:http` listener. The logger receives method, path, status,
 * and duration only: no bodies, headers, tokens, addresses, or identifiers
 * that would turn an operational log into a content channel.
 */
export function createListener(
  router: Router,
  log: (entry: RequestLog) => void,
): (request: IncomingMessage, response: ServerResponse) => void {
  return (request, response) => {
    const startedAt = process.hrtime.bigint();
    const method = request.method ?? 'GET';
    const [rawPath, rawQuery] = (request.url ?? '/').split('?', 2);
    const path = rawPath || '/';
    const query = new URLSearchParams(rawQuery ?? '');

    const send = (status: number, body: unknown): void => {
      const payload = body === null ? '' : JSON.stringify(body);
      response.writeHead(status, {
        'content-type': 'application/json; charset=utf-8',
        'content-length': Buffer.byteLength(payload).toString(),
        'cache-control': 'no-store',
        'x-content-type-options': 'nosniff',
        'referrer-policy': 'no-referrer',
      });
      response.end(payload);
      log({
        method,
        path,
        status,
        durationMs: Number(process.hrtime.bigint() - startedAt) / 1_000_000,
      });
    };

    void (async () => {
      try {
        const matched = router.match(method, path);
        if (!matched) {
          throw new HttpError(404, 'not_found', 'no such resource');
        }
        const body = method === 'GET' || method === 'DELETE' ? null : await readBody(request);
        const reply = await matched.handler({
          method,
          path,
          params: matched.params,
          query,
          headers: request.headers,
          body,
        });
        send(reply.status, reply.body);
      } catch (error) {
        if (error instanceof HttpError) {
          // `error` is a stable machine code and `reason` a human sentence.
          // The Swift and TypeScript clients both read this shape.
          send(error.status, {
            error: error.code,
            reason: error.message,
            field: error.field ?? undefined,
          });
          return;
        }
        // Unexpected failures must not echo internals to the caller. Only the
        // error class name reaches stderr; a message could carry a value.
        process.stderr.write(
          `${JSON.stringify({ level: 'error', message: 'unhandled request failure', name: (error as Error).name })}\n`,
        );
        send(500, { error: 'internal_error', reason: 'internal error' });
      }
    })();
  };
}
