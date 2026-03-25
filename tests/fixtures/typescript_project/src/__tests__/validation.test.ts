/**
 * Jest tests for validation utilities.
 * Demonstrates: Jest tests, schema validation, edge cases.
 */

import {
  createUserSchema,
  updateUserSchema,
  paginationSchema,
  idParamSchema,
  validateOrThrow,
} from "../utils/validation.js";

describe("Validation Schemas", () => {
  describe("createUserSchema", () => {
    it("should validate a correct input", () => {
      const result = createUserSchema.safeParse({
        name: "Alice",
        email: "alice@example.com",
      });

      expect(result.success).toBe(true);
    });

    it("should reject empty name", () => {
      const result = createUserSchema.safeParse({
        name: "",
        email: "alice@example.com",
      });

      expect(result.success).toBe(false);
    });

    it("should reject invalid email", () => {
      const result = createUserSchema.safeParse({
        name: "Alice",
        email: "not-an-email",
      });

      expect(result.success).toBe(false);
    });

    it("should allow optional role", () => {
      const result = createUserSchema.safeParse({
        name: "Alice",
        email: "alice@example.com",
        role: "admin",
      });

      expect(result.success).toBe(true);
    });

    it("should reject invalid role", () => {
      const result = createUserSchema.safeParse({
        name: "Alice",
        email: "alice@example.com",
        role: "superuser",
      });

      expect(result.success).toBe(false);
    });
  });

  describe("updateUserSchema", () => {
    it("should allow partial updates", () => {
      const result = updateUserSchema.safeParse({ name: "Bob" });
      expect(result.success).toBe(true);
    });

    it("should allow empty object", () => {
      const result = updateUserSchema.safeParse({});
      expect(result.success).toBe(true);
    });
  });

  describe("paginationSchema", () => {
    it("should apply defaults", () => {
      const result = paginationSchema.parse({});
      expect(result.page).toBe(1);
      expect(result.pageSize).toBe(20);
    });

    it("should coerce string numbers", () => {
      const result = paginationSchema.parse({ page: "3", pageSize: "10" });
      expect(result.page).toBe(3);
      expect(result.pageSize).toBe(10);
    });

    it("should reject pageSize over 100", () => {
      const result = paginationSchema.safeParse({ pageSize: 200 });
      expect(result.success).toBe(false);
    });
  });

  describe("idParamSchema", () => {
    it("should accept valid UUID", () => {
      const result = idParamSchema.safeParse({
        id: "550e8400-e29b-41d4-a716-446655440000",
      });
      expect(result.success).toBe(true);
    });

    it("should reject non-UUID string", () => {
      const result = idParamSchema.safeParse({ id: "not-a-uuid" });
      expect(result.success).toBe(false);
    });
  });

  describe("validateOrThrow", () => {
    it("should return parsed data on success", () => {
      const data = validateOrThrow(createUserSchema, {
        name: "Alice",
        email: "alice@example.com",
      });

      expect(data.name).toBe("Alice");
    });

    it("should throw on validation failure", () => {
      expect(() =>
        validateOrThrow(createUserSchema, { name: "", email: "" }),
      ).toThrow("Validation failed");
    });
  });
});
