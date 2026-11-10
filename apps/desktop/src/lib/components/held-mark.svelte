<script lang="ts">
  // The C · Held mark (`apps/shared/brand/held.svg`): one unbroken
  // line with a node at each peak — three devices, one chain. Amber
  // on transparent for Ink backgrounds (`mark`), or Amber on an Ink
  // tile (`tile`, the favicon shape). Colors follow the `@theme`
  // tokens in `src/app.css`, falling back to the canonical hex.

  interface Props {
    variant?: "mark" | "tile";
    size?: number;
    label?: string;
  }

  let { variant = "mark", size = 26, label }: Props = $props();

  const MARK = "M48 62 L88 211.3 L128 62 L168 211.3 L208 62";
  const INK = "var(--color-ink, #121614)";
  const AMBER = "var(--color-amber, #E3A13B)";
</script>

<svg
  width={size}
  height={size}
  viewBox="0 0 256 256"
  role={label ? "img" : undefined}
  aria-label={label}
  aria-hidden={label ? undefined : "true"}
>
  {#if variant === "tile"}
    <rect x="8" y="8" width="240" height="240" rx="56" style:fill={INK} />
  {/if}
  <path
    d={MARK}
    fill="none"
    style:stroke={AMBER}
    stroke-width="16"
    stroke-linecap="round"
    stroke-linejoin="round"
  />
  <circle cx="48" cy="62" r="22" style:fill={AMBER} />
  <circle cx="128" cy="62" r="22" style:fill={AMBER} />
  <circle cx="208" cy="62" r="22" style:fill={AMBER} />
</svg>
