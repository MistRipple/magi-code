import { mount } from 'svelte';
import '../styles/global.css';
import './composer-stats-preview.css';
import ComposerStatsPreview from './ComposerStatsPreview.svelte';

mount(ComposerStatsPreview, { target: document.querySelector('#composer-stats-preview')! });
