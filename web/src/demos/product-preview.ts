import { mount } from 'svelte';
import './card-preview.css';
import './product-preview.css';
import ProductWorkbench from './ProductWorkbench.svelte';

mount(ProductWorkbench, { target: document.querySelector('#product-preview')! });
