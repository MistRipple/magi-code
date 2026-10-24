// Development-only acceptance entry. Render the production components, never a UI replica.
import { mount } from 'svelte';
import '../styles/global.css';
import '../styles/tool-card.css';
import ToolCardAcceptance from './ToolCardAcceptance.svelte';
mount(ToolCardAcceptance, { target: document.querySelector('#app')! });
