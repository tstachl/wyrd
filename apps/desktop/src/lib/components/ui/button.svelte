<script lang="ts" module>
  import { tv, type VariantProps } from "tailwind-variants";

  export const buttonVariants = tv({
    base: "inline-flex shrink-0 items-center justify-center gap-2 rounded-md text-sm font-medium transition-colors outline-none focus-visible:ring-2 focus-visible:ring-neutral-500 disabled:pointer-events-none disabled:opacity-50 [&_svg]:size-4 [&_svg]:shrink-0",
    variants: {
      variant: {
        default: "bg-neutral-900 text-neutral-50 hover:bg-neutral-700",
        secondary: "bg-neutral-200 text-neutral-900 hover:bg-neutral-300",
        ghost: "text-neutral-900 hover:bg-neutral-200",
      },
      size: {
        default: "h-9 px-4 py-2",
        sm: "h-8 px-3 text-xs",
      },
    },
    defaultVariants: {
      variant: "default",
      size: "default",
    },
  });

  export type ButtonVariant = VariantProps<typeof buttonVariants>["variant"];
  export type ButtonSize = VariantProps<typeof buttonVariants>["size"];
</script>

<script lang="ts">
  import type { Snippet } from "svelte";
  import { cn } from "$lib/utils.js";

  interface Props {
    variant?: ButtonVariant;
    size?: ButtonSize;
    type?: "button" | "submit";
    disabled?: boolean;
    title?: string;
    class?: string;
    onclick?: (event: MouseEvent) => void;
    children: Snippet;
  }

  let {
    variant = "default",
    size = "default",
    type = "button",
    disabled = false,
    title,
    class: className,
    onclick,
    children,
  }: Props = $props();
</script>

<button
  {type}
  {disabled}
  {title}
  class={cn(buttonVariants({ variant, size }), className)}
  onclick={onclick}
>
  {@render children()}
</button>
