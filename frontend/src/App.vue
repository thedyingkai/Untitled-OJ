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
    primaryColor: '#0f766e',
    primaryColorHover: '#0b8178',
    primaryColorPressed: '#0b5f59',
    primaryColorSuppl: '#0f766e',
    borderRadius: '8px',
    borderRadiusSmall: '6px',
    fontFamily:
      "Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif",
  },
  Button: {
    borderRadiusMedium: '8px',
    borderRadiusSmall: '7px',
    fontWeight: '650',
  },
  Card: {
    borderRadius: '8px',
    titleFontWeight: '700',
  },
  DataTable: {
    thColor: '#f8fafb',
    thColorHover: '#f8fafb',
    borderColor: '#e8edf2',
    tdColorHover: '#fbfcfd',
    fontSizeMedium: '13px',
  },
  Menu: {
    itemTextColorActive: '#0b5f59',
    itemTextColorActiveHover: '#0b5f59',
    itemIconColorActive: '#0b5f59',
    itemColorActive: '#eef7f5',
    itemColorActiveHover: '#eef7f5',
    itemColorHover: '#f4f7f8',
    borderRadius: '8px',
  },
}
</script>
