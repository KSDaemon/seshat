// TSX component file — ensures JSX doesn't break parsing
import React, { useState, useEffect } from 'react';
import type { User } from './types';
import { UserService } from './services';

interface AppProps {
    title: string;
    debug?: boolean;
}

export const App: React.FC<AppProps> = ({ title, debug }) => {
    const [users, setUsers] = useState<User[]>([]);
    const [loading, setLoading] = useState(true);

    useEffect(() => {
        const service = new UserService();
        service.getAll().then((data) => {
            setUsers(data);
            setLoading(false);
        });
    }, []);

    if (loading) {
        return <div>Loading...</div>;
    }

    return (
        <div className="app">
            <h1>{title}</h1>
            {debug && <pre>{JSON.stringify(users, null, 2)}</pre>}
            <ul>
                {users.map((user) => (
                    <li key={user.id}>{user.name}</li>
                ))}
            </ul>
        </div>
    );
};

export default App;
