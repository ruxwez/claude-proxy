<script>
  import { Dialog } from 'bits-ui';
  import { X } from '@lucide/svelte';
  let { open = $bindable(false), title, description, children, wide = false } = $props();
</script>

<Dialog.Root bind:open>
  <Dialog.Portal>
    <Dialog.Overlay class="overlay" />
    <Dialog.Content class={`modal ${wide ? 'max-w-2xl' : ''}`}>
      <div class="mb-1 flex items-start justify-between gap-4">
        <Dialog.Title class="text-lg font-semibold tracking-tight">{title}</Dialog.Title>
        <Dialog.Close class="icon-btn -mr-2 -mt-2" aria-label="Cerrar"><X size={17} /></Dialog.Close>
      </div>
      <Dialog.Description class="mb-6 text-xs leading-relaxed text-muted">{description}</Dialog.Description>
      {@render children()}
    </Dialog.Content>
  </Dialog.Portal>
</Dialog.Root>
