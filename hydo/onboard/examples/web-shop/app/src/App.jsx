import { useCallback, useEffect, useState } from 'react';
import * as api from './api.js';

const fmt = (cents) => `$${(cents / 100).toFixed(2)}`;
const newCart = () => ({ id: `cart-${Date.now()}-${Math.floor(Math.random() * 1e6)}`, items: [] });

function loadCart() {
  try {
    return JSON.parse(localStorage.getItem('cart')) || newCart();
  } catch {
    return newCart();
  }
}

// a small router: the path in the address bar, and go(path) without a page load
function usePath() {
  const [path, setPath] = useState(location.pathname);
  useEffect(() => {
    const back = () => setPath(location.pathname);
    addEventListener('popstate', back);
    return () => removeEventListener('popstate', back);
  }, []);
  const go = useCallback((to) => {
    history.pushState(null, '', to);
    setPath(to);
  }, []);
  return [path, go];
}

function Link({ to, go, children }) {
  return (
    <a
      href={to}
      onClick={(e) => {
        e.preventDefault();
        go(to);
      }}
    >
      {children}
    </a>
  );
}

function Spinner({ children }) {
  return (
    <div className="spinner" aria-busy="true">
      {children}
    </div>
  );
}

function Products({ setCart, toast }) {
  const [products, setProducts] = useState(null);
  useEffect(() => {
    api.getProducts().then(setProducts, (e) => {
      toast(`We could not load the products (${e.message}).`);
      setProducts([]);
    });
  }, [toast]);
  async function add(p) {
    try {
      await api.addItem(p.id);
      setCart((c) => ({ ...c, items: [...c.items, p] }));
    } catch (e) {
      toast(`${p.name} was not added to your cart (${e.message}).`);
    }
  }
  if (!products) return <Spinner>Loading products…</Spinner>;
  return (
    <ul className="products">
      {products.map((p) => (
        <li key={p.id}>
          <span data-testid="product-name">{p.name}</span> <span>{fmt(p.price)}</span>{' '}
          <button aria-label={`Add ${p.name} to cart`} onClick={() => add(p)}>
            Add to cart
          </button>
        </li>
      ))}
    </ul>
  );
}

function Checkout({ cart, setCart, go, toast }) {
  const [busy, setBusy] = useState(false);
  const total = cart.items.reduce((s, p) => s + p.price, 0);
  async function place() {
    setBusy(true);
    try {
      const order = await api.placeOrder({ id: cart.id, items: cart.items, total });
      setCart(newCart());
      go(`/orders/${order.id}`);
    } catch (e) {
      toast(`Your order was not placed (${e.message}). Please try again.`);
      setBusy(false);
    }
  }
  return (
    <section>
      <h1>Checkout</h1>
      <ul>
        {cart.items.map((p, i) => (
          <li key={i}>
            {p.name} {fmt(p.price)}
          </li>
        ))}
      </ul>
      <p data-testid="total">Total {fmt(total)}</p>
      <button onClick={place} disabled={busy || cart.items.length === 0}>
        Place order
      </button>
      {busy && <Spinner>Placing your order…</Spinner>}
    </section>
  );
}

function Order({ id, toast }) {
  const [order, setOrder] = useState(null);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    if (api.BUGGY) {
      // the planted bug: no error handling, so a failed answer leaves the spinner on the screen
      // and an unhandled promise rejection in the console
      api.getOrder(id).then(setOrder);
    } else {
      api.getOrder(id).then(setOrder, (e) => {
        setFailed(true);
        toast(`We could not load order ${id} (${e.message}).`);
      });
    }
  }, [id, toast]);
  if (failed) return <p>Order {id} could not be loaded.</p>;
  if (!order) return <Spinner>Loading your order…</Spinner>;
  return (
    <section>
      <h1 data-testid="order-title">Order {order.id} confirmed</h1>
      <p>Total {fmt(order.total)}</p>
    </section>
  );
}

export default function App() {
  const [path, go] = usePath();
  const [cart, setCart] = useState(loadCart);
  const [error, setError] = useState(null);
  useEffect(() => localStorage.setItem('cart', JSON.stringify(cart)), [cart]);
  const toast = useCallback((message) => setError(message), []);
  let page;
  if (path === '/checkout') page = <Checkout cart={cart} setCart={setCart} go={go} toast={toast} />;
  else if (path.startsWith('/orders/')) page = <Order id={path.slice(8)} toast={toast} />;
  else page = <Products setCart={setCart} toast={toast} />;
  return (
    <>
      <header>
        <Link to="/" go={go}>
          Shop
        </Link>{' '}
        <Link to="/checkout" go={go}>
          Cart (<span data-testid="cart-count">{cart.items.length}</span>)
        </Link>
      </header>
      {error && (
        <div role="alert" className="toast">
          {error} <button onClick={() => setError(null)}>Dismiss</button>
        </div>
      )}
      <main>{page}</main>
    </>
  );
}
