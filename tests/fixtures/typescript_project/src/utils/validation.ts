/**
 * Zod-based validation schemas.
 * Demonstrates: zod usage, schema composition, type inference from schemas.
 */

import { z } from "zod";

import type { CreateUserInput, UpdateUserInput } from "../types/index.js";

export const emailSchema = z.string().email("Invalid email address").trim().toLowerCase();

export const userRoleSchema = z.enum(["admin", "editor", "viewer"]);

export const createUserSchema = z.object({
  name: z.string().min(1, "Name is required").max(100, "Name too long").trim(),
  email: emailSchema,
  role: userRoleSchema.optional(),
}) satisfies z.ZodType<CreateUserInput>;

export const updateUserSchema = z.object({
  name: z.string().min(1).max(100).trim().optional(),
  email: emailSchema.optional(),
  role: userRoleSchema.optional(),
  metadata: z.record(z.unknown()).optional(),
}) satisfies z.ZodType<UpdateUserInput>;

export const paginationSchema = z.object({
  page: z.coerce.number().int().positive().default(1),
  pageSize: z.coerce.number().int().min(1).max(100).default(20),
});

export const idParamSchema = z.object({
  id: z.string().uuid("Invalid ID format"),
});

export type CreateUserSchemaType = z.infer<typeof createUserSchema>;
export type UpdateUserSchemaType = z.infer<typeof updateUserSchema>;
export type PaginationParams = z.infer<typeof paginationSchema>;

export function validateOrThrow<T>(schema: z.ZodSchema<T>, data: unknown): T {
  const result = schema.safeParse(data);
  if (!result.success) {
    const fieldErrors: Record<string, string> = {};
    for (const issue of result.error.issues) {
      const path = issue.path.join(".");
      fieldErrors[path] = issue.message;
    }
    throw new Error(`Validation failed: ${JSON.stringify(fieldErrors)}`);
  }
  return result.data;
}
