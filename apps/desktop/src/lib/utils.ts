import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

/** shadcn-svelte `cn`: merge Tailwind classes without conflicts. */
export function cn(...inputs: ClassValue[]): string {
  return twMerge(clsx(inputs));
}

/* shadcn-svelte component prop helpers. Generated primitives import
   these from `$lib/utils.js`; keep the names and shapes in step with
   the registry so a re-generated component typechecks unchanged. */

/** Add a bindable element ref to an attribute type. */
export type WithElementRef<T, U extends HTMLElement = HTMLElement> = T & {
  ref?: U | null;
};

/** Drop a `child` snippet prop, where the attribute type declares one. */
export type WithoutChild<T> = T extends { child?: unknown } ? Omit<T, "child"> : T;

/** Drop a `children` snippet prop, where the attribute type declares one. */
export type WithoutChildren<T> = T extends { children?: unknown } ? Omit<T, "children"> : T;

/** Drop both snippet props: the component renders its own children. */
export type WithoutChildrenOrChild<T> = WithoutChildren<WithoutChild<T>>;
