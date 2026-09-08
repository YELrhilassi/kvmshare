import { clsx, type ClassValue } from "clsx"
import { twMerge } from "tailwind-merge"

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs))
}

// The human-facing short form of a machine id: its first 8 chars. The
// backend matches trusted ids by prefix, so pasting the short form works
// everywhere the full id would.
export function shortID(id: string): string {
  return id.length > 8 ? id.slice(0, 8) : id
}
