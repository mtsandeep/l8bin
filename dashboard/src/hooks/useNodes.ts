import { useEffect, useState } from 'react';
import { fetchNodes, type Node } from '../api';

export function useNodes(user: { username: string; is_admin: boolean } | null) {
  const [nodes, setNodes] = useState<Node[]>([]);

  useEffect(() => {
    if (!user) return;
    (async () => {
      try {
        const data = await fetchNodes();
        setNodes(data);
      } catch (e) {
        console.error('Failed to fetch nodes:', e);
      }
    })();
  }, [user]);

  return { nodes };
}
