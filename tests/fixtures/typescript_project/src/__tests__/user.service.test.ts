/**
 * Jest tests for UserService.
 * Demonstrates: Jest test patterns, describe/it blocks, beforeEach, expect assertions.
 */

import { UserService } from "../services/user.service.js";
import { NotFoundError, ConflictError } from "../errors/index.js";
import { UserRole } from "../types/index.js";

describe("UserService", () => {
  let service: UserService;

  beforeEach(() => {
    service = new UserService();
  });

  describe("createUser", () => {
    it("should create a user with default role", async () => {
      const user = await service.createUser({
        name: "Alice",
        email: "alice@example.com",
      });

      expect(user.name).toBe("Alice");
      expect(user.email).toBe("alice@example.com");
      expect(user.role).toBe(UserRole.Viewer);
      expect(user.id).toBeDefined();
    });

    it("should create a user with specified role", async () => {
      const user = await service.createUser({
        name: "Bob",
        email: "bob@example.com",
        role: UserRole.Admin,
      });

      expect(user.role).toBe(UserRole.Admin);
    });

    it("should throw ConflictError for duplicate email", async () => {
      await service.createUser({
        name: "Alice",
        email: "alice@example.com",
      });

      await expect(
        service.createUser({
          name: "Alice 2",
          email: "alice@example.com",
        }),
      ).rejects.toThrow(ConflictError);
    });
  });

  describe("getUserById", () => {
    it("should return user by id", async () => {
      const created = await service.createUser({
        name: "Charlie",
        email: "charlie@example.com",
      });

      const found = await service.getUserById(created.id);
      expect(found.name).toBe("Charlie");
      expect(found.email).toBe("charlie@example.com");
    });

    it("should throw NotFoundError for unknown id", async () => {
      await expect(
        service.getUserById("non-existent-id"),
      ).rejects.toThrow(NotFoundError);
    });
  });

  describe("updateUser", () => {
    it("should update user name", async () => {
      const created = await service.createUser({
        name: "Dave",
        email: "dave@example.com",
      });

      const updated = await service.updateUser(created.id, {
        name: "David",
      });

      expect(updated.name).toBe("David");
      expect(updated.email).toBe("dave@example.com");
    });

    it("should throw ConflictError for duplicate email on update", async () => {
      const user1 = await service.createUser({
        name: "Eve",
        email: "eve@example.com",
      });
      await service.createUser({
        name: "Frank",
        email: "frank@example.com",
      });

      await expect(
        service.updateUser(user1.id, { email: "frank@example.com" }),
      ).rejects.toThrow(ConflictError);
    });
  });

  describe("deleteUser", () => {
    it("should delete existing user", async () => {
      const created = await service.createUser({
        name: "Grace",
        email: "grace@example.com",
      });

      await service.deleteUser(created.id);

      await expect(
        service.getUserById(created.id),
      ).rejects.toThrow(NotFoundError);
    });

    it("should throw NotFoundError for unknown id", async () => {
      await expect(
        service.deleteUser("non-existent-id"),
      ).rejects.toThrow(NotFoundError);
    });
  });

  describe("listUsers", () => {
    it("should return paginated users", async () => {
      for (let i = 0; i < 5; i++) {
        await service.createUser({
          name: `User ${i}`,
          email: `user${i}@example.com`,
        });
      }

      const result = await service.listUsers(1, 3);

      expect(result.items).toHaveLength(3);
      expect(result.total).toBe(5);
    });

    it("should return empty list for empty store", async () => {
      const result = await service.listUsers();

      expect(result.items).toHaveLength(0);
      expect(result.total).toBe(0);
    });
  });
});
