import js from '@eslint/js';
import ts from 'typescript-eslint';
import jsxA11y from 'eslint-plugin-jsx-a11y';

export default ts.config(
  js.configs.recommended,
  ts.configs.recommended,
  {
    files: ['**/*.{ts,tsx}'],
    languageOptions: {
      parser: ts.parser,
      parserOptions: {
        ecmaFeatures: { jsx: true },
      },
      globals: {
        document: 'readonly',
        window: 'readonly',
        localStorage: 'readonly',
        navigator: 'readonly',
      },
    },
    plugins: {
      'jsx-a11y': jsxA11y,
    },
    rules: {
      ...jsxA11y.configs.recommended.rules,
      '@typescript-eslint/no-explicit-any': 'off',
      '@typescript-eslint/no-unused-vars': ['error', { argsIgnorePattern: '^_', varsIgnorePattern: '^_' }],
    },
  },
  {
    files: ['src/*.js'],
    languageOptions: {
      globals: {
        window: 'readonly',
        setTimeout: 'readonly',
        clearTimeout: 'readonly',
      },
    },
  }
);
