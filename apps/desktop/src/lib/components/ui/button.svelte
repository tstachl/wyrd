<script lang="ts" module>
  import { tv, type VariantProps } from "tailwind-variants";

  export const buttonVariants = tv({
    base: "inline-flex shrink-0 items-center justify-center gap-2 rounded-md text-sm font-medium transition-colors outline-none focus-visible:ring-2 focus-visible:ring-amber disabled:pointer-events-none disabled:opacity-50 [&_svg]:size-4 [&_svg]:shrink-0",
    variants: {
      variant: {
        default: "bg-amber text-ink hover:bg-ink hover:text-bone",
        secondary: "bg-ink text-bone hover:bg-smoke",
        ghost: "text-ink hover:bg-ink/10",
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
  import type { HTMLButtonAttributes } from "svelte/elements";
  import { cn } from "$lib/utils.js";

  interface Props extends HTMLButtonAttributes {
    variant?: ButtonVariant;
    size?: ButtonSize;
    children: Snippet;
  }

  let {
    variant = "default",
    size = "default",
    type = "button",
    class: className,
    children,
    ...rest
  }: Props = $props();
</script>

<button type={type} class={cn(buttonVariants({ variant, size }), className)} {...rest}>
  {@render children()}
</button>
