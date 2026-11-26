import { createApp, defineComponent, h } from 'vue'
import { createRouter, createWebHistory, RouterLink, RouterView } from 'vue-router'
import ServiceStatus from './ServiceStatus.vue'
import './style.css'

const MissingPage = defineComponent({
  render: () => h('main', [
    h('h1', '페이지를 찾을 수 없습니다'),
    h(RouterLink, { to: '/' }, () => '서비스 상태 보기'),
  ]),
})

const router = createRouter({
  history: createWebHistory(),
  routes: [
    { path: '/', component: ServiceStatus },
    { path: '/:pathMatch(.*)*', component: MissingPage },
  ],
})

createApp(RouterView).use(router).mount('#app')
