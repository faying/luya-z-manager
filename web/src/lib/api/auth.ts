import { apiFetch, setToken, getToken } from "./client";

export async function login(password: string): Promise<string> {
  const data = await apiFetch<{ token: string }>("/api/auth/login", {
    method: "POST",
    body: { password },
    noAuth: true,
  });
  setToken(data.token);
  return data.token;
}

export function logout() {
  setToken(null);
}

export function isAuthed(): boolean {
  return !!getToken();
}
