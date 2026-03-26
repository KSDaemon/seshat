// Service layer with classes, decorators, and dependency injection
import type { User, UserCreateInput } from './types';

export interface Repository<T> {
    findAll(): Promise<T[]>;
    findById(id: string): Promise<T | null>;
    create(data: Partial<T>): Promise<T>;
    delete(id: string): Promise<void>;
}

@Injectable()
@Singleton
export class UserService {
    constructor(private readonly repo: Repository<User>) {}

    async getAll(): Promise<User[]> {
        return this.repo.findAll();
    }

    async getById(id: string): Promise<User | null> {
        return this.repo.findById(id);
    }

    async create(input: UserCreateInput): Promise<User> {
        return this.repo.create(input);
    }

    async remove(id: string): Promise<void> {
        return this.repo.delete(id);
    }
}

export default UserService;

function validateEmail(email: string): boolean {
    return email.includes('@');
}

const formatName = (first: string, last: string): string => {
    return `${first} ${last}`;
};
