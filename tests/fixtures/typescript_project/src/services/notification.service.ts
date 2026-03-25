/**
 * Notification service for sending alerts.
 * Demonstrates: interface-based design, enum usage, async patterns, type-only imports.
 */

import type { UserId } from "../types/index.js";

import { logger } from "../utils/logger.js";

export enum NotificationChannel {
  Email = "email",
  Sms = "sms",
  Push = "push",
}

export interface NotificationPayload {
  recipient: UserId;
  channel: NotificationChannel;
  subject: string;
  body: string;
  metadata?: Record<string, unknown>;
}

export interface NotificationResult {
  id: string;
  sent: boolean;
  channel: NotificationChannel;
  timestamp: Date;
  error?: string;
}

export interface NotificationProvider {
  send(payload: NotificationPayload): Promise<NotificationResult>;
  supports(channel: NotificationChannel): boolean;
}

export class NotificationService {
  private readonly providers: NotificationProvider[] = [];

  public registerProvider(provider: NotificationProvider): void {
    this.providers.push(provider);
    logger.info(`Registered notification provider`);
  }

  public async sendNotification(
    payload: NotificationPayload,
  ): Promise<NotificationResult> {
    const provider = this.providers.find((p) => p.supports(payload.channel));

    if (!provider) {
      logger.warn(`No provider found for channel: ${payload.channel}`);
      return {
        id: "",
        sent: false,
        channel: payload.channel,
        timestamp: new Date(),
        error: `No provider for channel: ${payload.channel}`,
      };
    }

    try {
      const result = await provider.send(payload);
      logger.info(`Notification sent via ${payload.channel}: ${result.id}`);
      return result;
    } catch (error) {
      const message = error instanceof Error ? error.message : "Unknown error";
      logger.error(`Failed to send notification: ${message}`);
      return {
        id: "",
        sent: false,
        channel: payload.channel,
        timestamp: new Date(),
        error: message,
      };
    }
  }

  public async sendBulk(
    payloads: NotificationPayload[],
  ): Promise<NotificationResult[]> {
    return Promise.all(payloads.map((p) => this.sendNotification(p)));
  }
}
