// Type definitions for the project

export interface User {
    id: string;
    name: string;
    email: string;
    role: UserRole;
    createdAt: Date;
}

export interface UserCreateInput {
    name: string;
    email: string;
    role?: UserRole;
}

export type UserRole = 'admin' | 'editor' | 'viewer';

export type ID = string | number;

export enum Status {
    Active = 'active',
    Inactive = 'inactive',
    Pending = 'pending',
}

type InternalConfig = {
    dbUrl: string;
    port: number;
};
