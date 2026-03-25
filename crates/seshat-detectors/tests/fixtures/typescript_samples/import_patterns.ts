// @ts-nocheck
// Sample: ESM import patterns and type-only imports
// Expected detections: ESM imports, type-only imports, grouped imports, .js extensions

// Type-only imports (separate statement)
import type { User, UserId } from "../types/user.js";
import type { ApiResponse } from "../types/api.js";
import type { Request, Response, NextFunction } from "express";

// External package imports
import { z } from "zod";
import { v4 as uuidv4 } from "uuid";
import winston from "winston";

// Local module imports with .js extensions
import { UserService } from "../services/user.service.js";
import { NotFoundError, ValidationError } from "../errors/app-error.js";
import { logger } from "../utils/logger.js";

// Side-effect import
import "./setup.js";

const userSchema = z.object({
  name: z.string().min(1),
  email: z.string().email(),
});

export async function handleGetUser(
  req: Request,
  res: Response,
  next: NextFunction,
): Promise<void> {
  const service = new UserService();
  const id: UserId = req.params.id;

  try {
    const user: User = await service.getUserById(id);
    const response: ApiResponse<User> = {
      success: true,
      data: user,
      timestamp: new Date().toISOString(),
    };
    res.json(response);
  } catch (error) {
    if (error instanceof NotFoundError) {
      res.status(404).json({ error: error.message });
    } else {
      next(error);
    }
  }
}

export function generateId(): string {
  return uuidv4();
}
