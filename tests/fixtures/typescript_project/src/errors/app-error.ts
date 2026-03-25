/**
 * Custom error classes for the application.
 * Demonstrates: custom error hierarchy, extending Error, error codes.
 */

export class AppError extends Error {
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
}

export class NotFoundError extends AppError {
  constructor(resource: string, id: string) {
    super(`${resource} with id '${id}' not found`, "NOT_FOUND", 404);
  }
}

export class ValidationError extends AppError {
  public readonly fields: Record<string, string>;

  constructor(message: string, fields: Record<string, string> = {}) {
    super(message, "VALIDATION_ERROR", 400);
    this.fields = fields;
  }
}

export class ConflictError extends AppError {
  constructor(message: string) {
    super(message, "CONFLICT", 409);
  }
}

export class UnauthorizedError extends AppError {
  constructor(message: string = "Unauthorized") {
    super(message, "UNAUTHORIZED", 401);
  }
}

export function isAppError(error: unknown): error is AppError {
  return error instanceof AppError;
}

export function toApiErrorResponse(error: unknown): {
  code: string;
  message: string;
  statusCode: number;
} {
  if (isAppError(error)) {
    return {
      code: error.code,
      message: error.message,
      statusCode: error.statusCode,
    };
  }

  return {
    code: "INTERNAL_ERROR",
    message: "An unexpected error occurred",
    statusCode: 500,
  };
}
