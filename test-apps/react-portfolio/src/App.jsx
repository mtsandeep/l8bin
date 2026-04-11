import { Routes, Route } from "react-router-dom";
import { useLocalStorage } from "./hooks/useLocalStorage";
import { initialPosts } from "./data/seed";
import Layout from "./components/Layout";
import Home from "./pages/Home";
import Projects from "./pages/Projects";
import Blog from "./pages/Blog";
import PostDetail from "./pages/PostDetail";
import NewPost from "./pages/NewPost";
import About from "./pages/About";
import Contact from "./pages/Contact";

export default function App() {
  const [posts, setPosts] = useLocalStorage("sutra-posts", initialPosts);

  const addPost = (post) => {
    setPosts((prev) => [{ ...post, id: slugify(post.title), date: new Date().toISOString().slice(0, 10) }, ...prev]);
  };

  const deletePost = (id) => {
    setPosts((prev) => prev.filter((p) => p.id !== id));
  };

  return (
    <Layout>
      <Routes>
        <Route path="/" element={<Home posts={posts} />} />
        <Route path="/projects" element={<Projects />} />
        <Route path="/blog" element={<Blog posts={posts} />} />
        <Route path="/blog/new" element={<NewPost onAdd={addPost} />} />
        <Route path="/blog/:id" element={<PostDetail posts={posts} onDelete={deletePost} />} />
        <Route path="/about" element={<About />} />
        <Route path="/contact" element={<Contact />} />
      </Routes>
    </Layout>
  );
}

function slugify(text) {
  return text
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/(^-|-$)/g, "")
    + "-" + Math.random().toString(36).slice(2, 6);
}
