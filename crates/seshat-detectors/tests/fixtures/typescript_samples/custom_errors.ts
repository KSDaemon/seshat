// @ts-nocheck
// Sample: custom error class patterns
// Expected detections: error class hierarchy, extends Error, error codes, type guards

export class BaseError extends Error {
  public readonly code: string;
  public readonly statusCode: number;
  public readonly isOperational: boolean;

  constructor(message: string, code: string, statusCode: number = 500) {
    super(message);
    this.name = this.constructor.name;
    this.code = code;
    this.statusCode = statusCode;
    this.isOperational = true;
    Error.captureStackTrace(this, this.constructor);
  }

  public toJSON(): Record<string, unknown> {
    return {
      name: this.name,
      code: this.code,
      message: this.message,
      statusCode: this.statusCode,
    };
  }
}

export class NotFoundError extends BaseError {
  constructor(resource: string, id: string) {
    super(`${resource} '${id}' not found`, "NOT_FOUND", 404);
  }
}

export class ValidationError extends BaseError {
  public readonly fields: Record<string, string>;

  constructor(message: string, fields: Record<string, string> = {}) {
    super(message, "VALIDATION_ERROR", 400);
    this.fields = fields;
  }
}

export class AuthenticationError extends BaseError {
  constructor(message: string = "Authentication required") {
    super(message, "AUTHENTICATION_ERROR", 401);
  }
}

export class AuthorizationError extends BaseError {
  constructor(message: string = "Insufficient permissions") {
    super(message, "AUTHORIZATION_ERROR", 403);
  }
}

export class RateLimitError extends BaseError {
  public readonly retryAfter: number;

  constructor(retryAfter: number = 60) {
    super("Too many requests", "RATE_LIMIT_EXCEEDED", 429);
    this.retryAfter = retryAfter;
  }
}

// Type guard function
export function isBaseError(error: unknown): error is BaseError {
  return error instanceof BaseError;
}

// Error handler
export function formatError(error: unknown): {
  code: string;
  message: string;
  statusCode: number;
} {
  if (isBaseError(error)) {
    return {
      code: error.code,
      message: error.message,
      statusCode: error.statusCode,
    };
  }

  if (error instanceof Error) {
    return {
      code: "INTERNAL_ERROR",
      message: error.message,
      statusCode: 500,
    };
  }

  return {
    code: "UNKNOWN_ERROR",
    message: "An unknown error occurred",
    statusCode: 500,
  };
}
