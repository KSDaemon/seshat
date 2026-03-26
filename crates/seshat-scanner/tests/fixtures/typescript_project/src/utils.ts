// Utility functions — mix of exported and non-exported

import * as crypto from 'crypto';

export function generateId(): string {
    return crypto.randomUUID();
}

export async function delay(ms: number): Promise<void> {
    return new Promise((resolve) => setTimeout(resolve, ms));
}

export const pipe = <T>(...fns: Array<(arg: T) => T>) => (value: T): T =>
    fns.reduce((acc, fn) => fn(acc), value);

function internalHelper(): void {
    // Not exported
}

const privateFormatter = (s: string) => s.trim();

export type { User } from './types';
