import { mount } from 'svelte';
import './card-preview.css';
import IconLibrary from './IconLibrary.svelte';

mount(IconLibrary, { target: document.querySelector('#icon-library')! });
