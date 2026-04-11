import { useEffect, useRef } from "react";

export interface DesktopNotificationPayload {
  title: string;
  body: string;
}

const EVENT_NAME = "caduceus:notify";
const PERMISSION_KEY = "caduceus:notifications-requested";

export function emitDesktopNotification(payload: DesktopNotificationPayload) {
  window.dispatchEvent(new CustomEvent<DesktopNotificationPayload>(EVENT_NAME, { detail: payload }));
}

export default function DesktopNotifications() {
  const requestedRef = useRef(false);

  useEffect(() => {
    const ensurePermission = async () => {
      if (!("Notification" in window)) return;
      if (Notification.permission !== "default") return;
      if (requestedRef.current) return;
      if (window.localStorage.getItem(PERMISSION_KEY) === "true") return;
      requestedRef.current = true;
      window.localStorage.setItem(PERMISSION_KEY, "true");
      await Notification.requestPermission();
    };

    void ensurePermission();

    const handleNotification = async (event: Event) => {
      const customEvent = event as CustomEvent<DesktopNotificationPayload>;
      if (!("Notification" in window)) return;
      if (Notification.permission === "default") {
        await Notification.requestPermission();
      }
      if (Notification.permission === "granted") {
        new Notification(customEvent.detail.title, { body: customEvent.detail.body });
      }
    };

    window.addEventListener(EVENT_NAME, handleNotification as EventListener);
    return () => window.removeEventListener(EVENT_NAME, handleNotification as EventListener);
  }, []);

  return null;
}
