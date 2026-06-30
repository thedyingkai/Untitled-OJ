<template>
  <NConfigProvider :theme-overrides="themeOverrides">
    <NDialogProvider>
      <NMessageProvider>
        <RouterView />
      </NMessageProvider>
    </NDialogProvider>
  </NConfigProvider>
</template>

<script setup lang="ts">
import { NConfigProvider, NDialogProvider, NMessageProvider } from 'naive-ui'

const themeOverrides = {
  common: {
    primaryColor: '#0b4f8a',
    primaryColorHover: '#0d63a8',
    primaryColorPressed: '#083b66',
    primaryColorSuppl: '#0b4f8a',
    borderRadius: '4px',
    borderRadiusSmall: '3px',
    fontFamily:
      "Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif",
  },
  Button: {
    borderRadiusMedium: '4px',
    borderRadiusSmall: '3px',
    fontWeight: '650',
  },
  Card: {
    borderRadius: '4px',
    titleFontWeight: '700',
  },
  DataTable: {
    thColor: '#e8eef5',
    thColorHover: '#e8eef5',
    borderColor: '#c8d2de',
    tdColorHover: '#f1f5f9',
    fontSizeMedium: '12px',
  },
  Menu: {
    itemTextColorActive: '#ffffff',
    itemTextColorActiveHover: '#ffffff',
    itemIconColorActive: '#ffffff',
    itemColorActive: '#0b4f8a',
    itemColorActiveHover: '#0b4f8a',
    itemColorHover: '#edf3f8',
    borderRadius: '4px',
  },
}
</script>
