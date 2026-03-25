/**
 * Abstract base model with common fields.
 * Demonstrates: abstract classes, protected members, generics.
 */

import { v4 as uuidv4 } from "uuid";

export abstract class BaseModel {
  public readonly id: string;
  public readonly createdAt: Date;
  public updatedAt: Date;

  protected constructor(id?: string) {
    this.id = id ?? uuidv4();
    this.createdAt = new Date();
    this.updatedAt = new Date();
  }

  protected markUpdated(): void {
    this.updatedAt = new Date();
  }

  public abstract toJSON(): Record<string, unknown>;
}

export interface Serializable {
  toJSON(): Record<string, unknown>;
}

export interface Identifiable {
  readonly id: string;
}

export type ModelId = string;
