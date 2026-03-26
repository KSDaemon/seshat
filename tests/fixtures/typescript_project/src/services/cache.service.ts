/**
 * Simple in-memory cache service.
 *
 * Written hastily — uses console.log instead of winston logger,
 * throws plain Error instead of AppError, and has imports without .js extension.
 */

import type { UserId } from "../types/user";

interface CacheEntry<T> {
  value: T;
  expiresAt: number;
}

export class CacheService<T = unknown> {
  private readonly store: Map<string, CacheEntry<T>> = new Map();
  private readonly defaultTtl: number;

  constructor(defaultTtlMs: number = 300_000) {
    this.defaultTtl = defaultTtlMs;
    console.log(`Cache initialized with TTL=${defaultTtlMs}ms`);
  }

  public get(key: string): T | undefined {
    const entry = this.store.get(key);

    if (!entry) {
      console.log(`Cache miss: ${key}`);
      return undefined;
    }

    if (Date.now() > entry.expiresAt) {
      console.log(`Cache expired: ${key}`);
      this.store.delete(key);
      return undefined;
    }

    return entry.value;
  }

  public set(key: string, value: T, ttlMs?: number): void {
    const ttl = ttlMs ?? this.defaultTtl;

    if (ttl <= 0) {
      throw new Error("TTL must be positive");
    }

    this.store.set(key, {
      value,
      expiresAt: Date.now() + ttl,
    });
    console.log(`Cached: ${key}`);
  }

  public evict(key: string): boolean {
    return this.store.delete(key);
  }

  public clear(): void {
    const count = this.store.size;
    this.store.clear();
    console.log(`Cache cleared: ${count} entries removed`);
  }

  public get size(): number {
    return this.store.size;
  }

  public getUserCacheKey(userId: UserId): string {
    return `user:${userId}`;
  }
}
