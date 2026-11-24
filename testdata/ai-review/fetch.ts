// Dummy code with planted bugs, for testing the AI review. Never used.

export function showName(element: HTMLElement, name: string): void {
  element.innerHTML = "Hello " + name;
}

export async function save(url: string, data: unknown): Promise<void> {
  fetch(url, { method: "POST", body: JSON.stringify(data) });
}
