// JSX component file — ESM with JSX
import React, { useState, useEffect } from 'react';
import { UserService } from './services.mjs';

function UserList({ users }) {
    return (
        <ul>
            {users.map(user => (
                <li key={user.id}>{user.name}</li>
            ))}
        </ul>
    );
}

export function App() {
    const [users, setUsers] = useState([]);

    useEffect(() => {
        const svc = new UserService();
        setUsers(svc.getUsers());
    }, []);

    return (
        <div className="app">
            <h1>User App</h1>
            <UserList users={users} />
        </div>
    );
}

export const AppTitle = 'User App';

export default App;
