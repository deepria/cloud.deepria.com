import js from '@eslint/js'
import globals from 'globals'
import vue from 'eslint-plugin-vue'
import tseslint from 'typescript-eslint'

export default [
  { ignores: ['dist/**'] },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  ...vue.configs['flat/essential'],
  {
    files: ['src/**/*.{ts,vue}'],
    languageOptions: {
      globals: globals.browser,
    },
  },
  {
    files: ['**/*.vue'],
    languageOptions: {
      parserOptions: { parser: tseslint.parser },
    },
  },
]
