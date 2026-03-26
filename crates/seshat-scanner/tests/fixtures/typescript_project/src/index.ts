// Main entry point — barrel exports and re-exports
import { UserService } from './services';
import type { User, UserRole } from './types';

export { UserService } from './services';
export * from './types';
export { default as App } from './app';

export async function main(): Promise<void> {
    const service = new UserService();
    const users = await service.getAll();
    console.log(`Found ${users.length} users`);
}

export const VERSION = '1.0.0';
