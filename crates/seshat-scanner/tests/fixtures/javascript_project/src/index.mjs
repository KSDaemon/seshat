// ESM entry point — .mjs forces ESM module system
import { UserService } from './services.mjs';
import { formatName, delay } from './utils.mjs';
import * as config from './config.mjs';

export { UserService } from './services.mjs';
export * from './constants.mjs';

export async function main() {
    const svc = new UserService();
    const name = formatName('John', 'Doe');
    await delay(100);
    return svc.greet(name);
}

export const VERSION = '2.0.0';

export default function bootstrap() {
    return main();
}
