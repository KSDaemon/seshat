// ESM services module
import { validateEmail } from './utils.mjs';

export class UserService {
    constructor() {
        this.users = [];
    }

    addUser(name, email) {
        if (!validateEmail(email)) {
            throw new Error('Invalid email');
        }
        this.users.push({ name, email });
    }

    getUsers() {
        return this.users;
    }
}

export class AdminService extends UserService {
    constructor() {
        super();
        this.admins = [];
    }
}

export function createService() {
    return new UserService();
}

const internalHelper = () => {
    return 'internal';
};

export default UserService;
