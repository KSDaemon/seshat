/**
 * Application logger configuration.
 * Demonstrates: winston logging setup, ESM imports, configuration patterns.
 */

import winston from "winston";

const LOG_LEVELS = {
  error: 0,
  warn: 1,
  info: 2,
  http: 3,
  debug: 4,
} as const;

const LOG_COLORS: Record<string, string> = {
  error: "red",
  warn: "yellow",
  info: "green",
  http: "magenta",
  debug: "white",
};

winston.addColors(LOG_COLORS);

const consoleFormat = winston.format.combine(
  winston.format.timestamp({ format: "YYYY-MM-DD HH:mm:ss" }),
  winston.format.colorize({ all: true }),
  winston.format.printf(
    (info) => `${info.timestamp} ${info.level}: ${info.message}`,
  ),
);

const jsonFormat = winston.format.combine(
  winston.format.timestamp(),
  winston.format.json(),
);

export const logger = winston.createLogger({
  levels: LOG_LEVELS,
  level: process.env.LOG_LEVEL ?? "info",
  transports: [
    new winston.transports.Console({
      format: consoleFormat,
    }),
  ],
});

export function createChildLogger(context: Record<string, unknown>): winston.Logger {
  return logger.child(context);
}

if (process.env.NODE_ENV === "production") {
  logger.add(
    new winston.transports.File({
      filename: "logs/error.log",
      level: "error",
      format: jsonFormat,
    }),
  );
  logger.add(
    new winston.transports.File({
      filename: "logs/combined.log",
      format: jsonFormat,
    }),
  );
}
