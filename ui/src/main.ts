import { mount } from 'svelte';
import App from './App.svelte';
import './app.css';
import { applyTheme, storedTheme } from './lib/theme';

applyTheme(storedTheme());

const target = document.getElementById('app');
if (!target) throw new Error('missing #app element');

export default mount(App, { target });
