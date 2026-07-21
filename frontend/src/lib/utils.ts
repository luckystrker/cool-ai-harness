import { type ClassValue, clsx } from "clsx"
import { twMerge } from "tailwind-merge"

/** Merge Tailwind classes, resolving conflicts. Used by all UI components. */
export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs))
}
