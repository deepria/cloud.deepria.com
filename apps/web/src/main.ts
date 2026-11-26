import { createApp } from 'vue'
import { createRouter, createWebHistory } from 'vue-router'
import App from './App.vue'
import MemoWorkspace from './MemoWorkspace.vue'
import './style.css'

const router = createRouter({
  history: createWebHistory(),
  routes: [
    { name: 'home', path: '/', component: { render: () => null } },
    { name: 'memos', path: '/memos/:id?', component: MemoWorkspace },
    { path: '/:pathMatch(.*)*', redirect: '/' },
  ],
})

createApp(App).use(router).mount('#app')
