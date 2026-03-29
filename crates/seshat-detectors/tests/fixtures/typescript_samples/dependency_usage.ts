// @ts-nocheck
// Sample: Dependency usage patterns for TypeScript
// Expected detections: canonical HTTP library (express), canonical testing library (jest),
// canonical validation library (zod), canonical database library (prisma)

import express, { Request, Response } from "express";
import { z } from "zod";
import { PrismaClient } from "@prisma/client";
import { describe, it, expect } from "jest";
import winston from "winston";

// Express HTTP server setup
const app = express();

const UserSchema = z.object({
  name: z.string().min(1),
  email: z.string().email(),
  age: z.number().int().positive(),
});

type User = z.infer<typeof UserSchema>;

const prisma = new PrismaClient();

const logger = winston.createLogger({
  level: "info",
  format: winston.format.json(),
  transports: [new winston.transports.Console()],
});

app.post("/users", async (req: Request, res: Response) => {
  const parsed = UserSchema.safeParse(req.body);
  if (!parsed.success) {
    logger.warn("Validation failed", { errors: parsed.error });
    return res.status(400).json({ error: parsed.error });
  }

  const user = await prisma.user.create({ data: parsed.data });
  logger.info("User created", { userId: user.id });
  return res.status(201).json(user);
});

app.get("/users/:id", async (req: Request, res: Response) => {
  const user = await prisma.user.findUnique({
    where: { id: req.params.id },
  });
  if (!user) {
    return res.status(404).json({ error: "Not found" });
  }
  return res.json(user);
});

export { app };
