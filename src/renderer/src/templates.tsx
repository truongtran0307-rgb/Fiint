/**
 * Ready-made requests for design & event work. Picking one types it into the open chat's input
 * (not sent): fill in the [ ] parts, then press Enter.
 */
import { Calculator, CalendarClock, ClipboardList, Handshake, Lightbulb, Megaphone, PenTool, ScrollText } from 'lucide-react'
import { useStore } from './store'
import { defaultAgent, newChat } from './actions'

export interface Template {
  key: string
  label: string
  hint: string
  icon: React.ReactNode
  text: string
}

export const TEMPLATES: Template[] = [
  {
    key: 'concept',
    label: 'Ý tưởng concept',
    hint: '3 concept: thông điệp, màu sắc, trang trí',
    icon: <Lightbulb size={16} />,
    text: 'Lên 3 concept cho sự kiện [tên sự kiện] của khách hàng [ngành nghề], quy mô [số khách] khách, ngân sách khoảng [số tiền]. Mỗi concept gồm: tên, thông điệp, màu sắc chủ đạo, phong cách trang trí, điểm nhấn trải nghiệm cho khách.'
  },
  {
    key: 'script',
    label: 'Kịch bản chương trình',
    hint: 'Từng mục, thời lượng, lời dẫn MC',
    icon: <ScrollText size={16} />,
    text: 'Viết kịch bản chi tiết cho chương trình [tên sự kiện] ngày [ngày], từ [giờ bắt đầu] đến [giờ kết thúc]: từng mục, thời lượng, người phụ trách, lời dẫn MC, yêu cầu âm thanh ánh sáng.'
  },
  {
    key: 'timeline',
    label: 'Timeline chuẩn bị',
    hint: 'Đầu việc, hạn chót, người phụ trách',
    icon: <CalendarClock size={16} />,
    text: 'Lập timeline chuẩn bị cho sự kiện [tên sự kiện] diễn ra ngày [ngày], tính ngược từ hôm nay: đầu việc, hạn chót, người phụ trách, các mốc duyệt với khách hàng.'
  },
  {
    key: 'budget',
    label: 'Dự toán chi phí',
    hint: 'Hạng mục, đơn giá, thành tiền',
    icon: <Calculator size={16} />,
    text: 'Lập bảng dự toán chi phí cho sự kiện [tên sự kiện], [số khách] khách, tại [địa điểm]: hạng mục, số lượng, đơn giá, thành tiền, dự phòng 10%. Lưu thành tệp Excel trong thư mục này.'
  },
  {
    key: 'checklist',
    label: 'Checklist hậu cần',
    hint: 'Trước, trong và sau sự kiện',
    icon: <ClipboardList size={16} />,
    text: 'Tạo checklist hậu cần cho sự kiện [tên sự kiện]: trước ngày diễn, ngày setup, ngày diễn, sau sự kiện. Chia theo hạng mục: sân khấu, âm thanh ánh sáng, in ấn, quà tặng, ăn uống, nhân sự.'
  },
  {
    key: 'media',
    label: 'Nội dung truyền thông',
    hint: 'Bài Facebook, thư mời, tin nhắn Zalo',
    icon: <Megaphone size={16} />,
    text: 'Viết nội dung truyền thông cho sự kiện [tên sự kiện]: 3 bài Facebook (trước, trong và sau sự kiện), thư mời khách, tin nhắn Zalo nhắc lịch.'
  },
  {
    key: 'brief',
    label: 'Brief thiết kế',
    hint: 'Nhận diện, ấn phẩm, kích thước',
    icon: <PenTool size={16} />,
    text: 'Viết brief thiết kế cho bộ nhận diện sự kiện [tên sự kiện]: thông điệp, đối tượng, màu sắc, font chữ, danh sách ấn phẩm cần làm (backdrop, standee, thư mời, banner online) kèm kích thước.'
  },
  {
    key: 'proposal',
    label: 'Proposal gửi khách',
    hint: 'Concept, hạng mục, báo giá, tiến độ',
    icon: <Handshake size={16} />,
    text: 'Soạn proposal gửi khách hàng [tên khách hàng] cho sự kiện [tên sự kiện]: giới thiệu, concept, hạng mục công việc, báo giá tóm tắt, tiến độ, điều khoản thanh toán.'
  }
]

/** Type a template into the open chat (or a new one), without sending it. */
export function insertTemplate(text: string): void {
  const { activeChatId, chats } = useStore.getState()
  const chat = chats.find((c) => c.id === activeChatId)
  if (chat && chat.status !== 'exited') {
    useStore.getState().patchChat(chat.id, { draft: chat.draft ? chat.draft + '\n\n' + text : text })
  } else if (defaultAgent()) newChat({ draft: text })
}
