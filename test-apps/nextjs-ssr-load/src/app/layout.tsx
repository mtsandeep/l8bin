import type { Metadata } from "next";
import "./globals.css";
import { TimingScript } from "./timing-script";

export const metadata: Metadata = {
  title: "Storefront — SSR Load Test",
  description: "Next.js SSR load testing application with realistic product data",
};

export default function RootLayout({
  children,
}: Readonly<{
  children: React.ReactNode;
}>) {
  return (
    <html lang="en">
      <body>
        <TimingScript />
        {children}
      </body>
    </html>
  );
}
