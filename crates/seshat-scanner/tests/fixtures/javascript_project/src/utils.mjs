// ESM utilities
import * as crypto from 'crypto';

export function generateId() {
    return crypto.randomUUID();
}

export async function delay(ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

export function formatName(first, last) {
    return `${first} ${last}`;
}

export function validateEmail(email) {
    return email.includes('@');
}

function internalHelper() {
    return 42;
}

const privateFormatter = (str) => str.trim();

export const MAX_RETRIES = 3;
